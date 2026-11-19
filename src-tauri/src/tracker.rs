//! Трекер «своего» аккаунта: ELO-лог, сессия за сегодня, итоги матчей, Discord-статус.
//! Работает в фоне через официальный Data API: раз в 90 с, в матче — раз в 25 с.

use crate::AppState;
use chrono::{Local, TimeZone};
use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashSet, fs, path::PathBuf, sync::Mutex, time::Duration};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::Notify;

const MAX_LOG: usize = 3000;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EloPoint {
    /// unix ms
    pub t: i64,
    pub elo: i64,
    #[serde(default)]
    pub match_id: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct Persisted {
    #[serde(default)]
    nickname: String,
    #[serde(default)]
    log: Vec<EloPoint>,
    #[serde(default)]
    seen: Vec<String>,
    #[serde(default)]
    last_match: Option<Value>,
}

#[derive(Serialize, Clone, Default)]
pub struct Today {
    pub wins: u32,
    pub losses: u32,
    pub delta: i64,
}

#[derive(Serialize, Clone)]
pub struct RecentMatch {
    pub match_id: String,
    pub finished_at: i64,
    pub win: Option<bool>,
}

#[derive(Serialize, Clone)]
pub struct Room {
    pub id: String,
    pub map: Option<String>,
    pub status: Option<String>,
    pub since: i64,
}

/// То, что видят панель, OBS-оверлей и Discord
#[derive(Serialize, Clone, Default)]
pub struct Snapshot {
    pub ok: bool,
    pub error: Option<String>,
    pub nickname: String,
    pub avatar: String,
    pub elo: Option<i64>,
    pub level: Option<i64>,
    pub today: Today,
    pub recent: Vec<RecentMatch>,
    pub log: Vec<EloPoint>,
    pub last_match: Option<Value>,
    pub room: Option<Room>,
    pub updated_at: i64,
}

struct Inner {
    persisted: Persisted,
    seen: HashSet<String>,
    player_id: Option<String>,
    initialized: bool,
    /// матч закончился, а ELO ещё не обновился
    pending_elo_match: Option<String>,
    room: Option<Room>,
    snap: Snapshot,
    discord: Option<DiscordIpcClient>,
    discord_id: String,
    discord_last: String,
}

pub struct Tracker {
    file: PathBuf,
    inner: Mutex<Inner>,
    wake: Notify,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Победил ли игрок в матче из /players/{id}/history
pub fn did_win(m: &Value, player_id: &str) -> Option<bool> {
    let winner = m["results"]["winner"].as_str()?;
    for f in ["faction1", "faction2"] {
        let players = m["teams"][f]["players"].as_array()?;
        if players.iter().any(|p| p["player_id"].as_str() == Some(player_id)) {
            return Some(f == winner);
        }
    }
    None
}

impl Tracker {
    pub fn new(dir: PathBuf) -> Self {
        let file = dir.join("history.json");
        let persisted: Persisted = fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let seen = persisted.seen.iter().cloned().collect();
        let snap = Snapshot {
            nickname: persisted.nickname.clone(),
            log: persisted.log.clone(),
            last_match: persisted.last_match.clone(),
            ..Default::default()
        };
        Self {
            file,
            inner: Mutex::new(Inner {
                initialized: !persisted.seen.is_empty(),
                persisted,
                seen,
                player_id: None,
                pending_elo_match: None,
                room: None,
                snap,
                discord: None,
                discord_id: String::new(),
                discord_last: String::new(),
            }),
            wake: Notify::new(),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.inner.lock().unwrap().snap.clone()
    }

    pub fn wake(&self) {
        self.wake.notify_one();
    }

    pub fn set_room(&self, id: String) {
        let mut g = self.inner.lock().unwrap();
        if g.room.as_ref().map(|r| r.id.as_str()) != Some(id.as_str()) {
            g.room = Some(Room { id, map: None, status: None, since: now_ms() });
        }
        drop(g);
        self.wake();
    }

    fn save(&self, g: &Inner) {
        let mut p = Persisted {
            nickname: g.persisted.nickname.clone(),
            log: g.persisted.log.clone(),
            seen: g.persisted.seen.clone(),
            last_match: g.persisted.last_match.clone(),
        };
        // ограничиваем размер
        if p.log.len() > MAX_LOG {
            p.log.drain(..p.log.len() - MAX_LOG);
        }
        if p.seen.len() > 500 {
            p.seen.drain(..p.seen.len() - 500);
        }
        if let Ok(json) = serde_json::to_string(&p) {
            let _ = fs::write(&self.file, json);
        }
    }

    fn in_room(&self) -> bool {
        let g = self.inner.lock().unwrap();
        g.room.as_ref().is_some_and(|r| now_ms() - r.since < 3 * 3600 * 1000)
    }
}

/// Сколько ждать до следующего тика
fn interval(app: &AppHandle) -> Duration {
    if app.state::<Tracker>().in_room() {
        Duration::from_secs(25)
    } else {
        Duration::from_secs(90)
    }
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        loop {
            tick(&app).await;
            let wait = interval(&app);
            let tracker = app.state::<Tracker>();
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = tracker.wake.notified() => {}
            }
        }
    });
}

fn emit(app: &AppHandle, snap: &Snapshot) {
    let _ = app.emit_to("stats", "tracker", snap);
}

async fn tick(app: &AppHandle) {
    let (key, nick, enabled, discord_id) = {
        let st = app.state::<AppState>();
        let c = st.cfg.lock().unwrap();
        (c.api_key.trim().to_string(), c.nickname.trim().to_string(), c.tracker_enabled, c.discord_client_id.trim().to_string())
    };
    let tracker = app.state::<Tracker>();
    if !enabled || key.is_empty() || nick.is_empty() {
        update_discord(&tracker, &discord_id);
        return;
    }

    // сменился ник — начинаем лог заново
    {
        let mut g = tracker.inner.lock().unwrap();
        if !g.persisted.nickname.eq_ignore_ascii_case(&nick) {
            g.persisted = Persisted { nickname: nick.clone(), ..Default::default() };
            g.seen.clear();
            g.player_id = None;
            g.initialized = false;
            g.pending_elo_match = None;
            g.snap = Snapshot { nickname: nick.clone(), ..Default::default() };
        }
    }

    let st = app.state::<AppState>();
    let player = match st.api.find_player(&key, &nick).await {
        Ok(p) => p,
        Err(e) => {
            let snap = {
                let mut g = tracker.inner.lock().unwrap();
                g.snap.ok = false;
                g.snap.error = Some(e);
                g.snap.clone()
            };
            emit(app, &snap);
            return;
        }
    };
    let pid = player["player_id"].as_str().unwrap_or_default().to_string();
    let elo = player["games"]["cs2"]["faceit_elo"].as_i64();
    let level = player["games"]["cs2"]["skill_level"].as_i64();

    let hist = st
        .api
        .get(&key, &format!("/players/{pid}/history"), &[("game", "cs2"), ("limit", "20")])
        .await
        .ok();
    let items: Vec<Value> = hist
        .as_ref()
        .and_then(|h| h["items"].as_array().cloned())
        .unwrap_or_default();
    let recent: Vec<RecentMatch> = items
        .iter()
        .filter(|m| m["status"].as_str().map_or(true, |s| s.eq_ignore_ascii_case("finished")))
        .filter_map(|m| {
            Some(RecentMatch {
                match_id: m["match_id"].as_str()?.to_string(),
                finished_at: m["finished_at"].as_i64().unwrap_or(0) * 1000,
                win: did_win(m, &pid),
            })
        })
        .collect();

    // новые законченные матчи (от старых к новым)
    let new_ids: Vec<String> = {
        let mut g = tracker.inner.lock().unwrap();
        g.player_id = Some(pid.clone());
        let fresh: Vec<String> = recent
            .iter()
            .rev()
            .filter(|m| !g.seen.contains(&m.match_id))
            .map(|m| m.match_id.clone())
            .collect();
        for id in &fresh {
            g.seen.insert(id.clone());
            g.persisted.seen.push(id.clone());
        }
        if g.initialized {
            fresh
        } else {
            // первый запуск: старые матчи не считаем «новыми»
            g.initialized = true;
            vec![]
        }
    };

    // итоги новых матчей
    let mut summaries = vec![];
    for mid in &new_ids {
        if let Some(s) = match_summary(app, &key, mid, &pid).await {
            summaries.push(s);
        }
    }

    // комната матча: карта / статус
    let room_id = tracker.inner.lock().unwrap().room.as_ref().map(|r| r.id.clone());
    if let Some(rid) = room_id {
        if new_ids.contains(&rid) {
            tracker.inner.lock().unwrap().room = None;
        } else if let Ok(m) = st.api.get(&key, &format!("/matches/{rid}"), &[]).await {
            let status = m["status"].as_str().map(String::from);
            let map = m["voting"]["map"]["pick"][0].as_str().map(String::from);
            let mut g = tracker.inner.lock().unwrap();
            let done = status
                .as_deref()
                .is_some_and(|s| matches!(s.to_uppercase().as_str(), "FINISHED" | "CANCELLED" | "ABORTED"));
            if done {
                g.room = None;
            } else if let Some(r) = g.room.as_mut() {
                r.status = status;
                if map.is_some() {
                    r.map = map;
                }
            }
        }
    }

    // ELO-лог + дельта для последнего матча
    let (snap, elo_delta_event) = {
        let mut g = tracker.inner.lock().unwrap();
        let mut delta_event = None;
        let last_elo = g.persisted.log.last().map(|p| p.elo);

        if let Some(newest) = summaries.last() {
            g.pending_elo_match = newest["match_id"].as_str().map(String::from);
        }
        for mut s in summaries {
            s["elo_delta"] = Value::Null;
            g.persisted.last_match = Some(s);
        }

        if let Some(elo) = elo {
            if last_elo != Some(elo) {
                let match_id = g.pending_elo_match.take();
                if let (Some(prev), Some(mid)) = (last_elo, match_id.clone()) {
                    let d = elo - prev;
                    if let Some(lm) = g.persisted.last_match.as_mut() {
                        if lm["match_id"].as_str() == Some(mid.as_str()) {
                            lm["elo_delta"] = json!(d);
                        }
                    }
                    delta_event = Some(json!({ "match_id": mid, "delta": d }));
                }
                g.persisted.log.push(EloPoint { t: now_ms(), elo, match_id });
            }
        }

        let today = compute_today(&g.persisted.log, &recent, elo);
        g.snap = Snapshot {
            ok: true,
            error: None,
            nickname: player["nickname"].as_str().unwrap_or(&nick).to_string(),
            avatar: player["avatar"].as_str().unwrap_or_default().to_string(),
            elo,
            level,
            today,
            recent,
            log: g.persisted.log.iter().rev().take(1000).rev().cloned().collect(),
            last_match: g.persisted.last_match.clone(),
            room: g.room.clone(),
            updated_at: now_ms(),
        };
        tracker.save(&g);
        (g.snap.clone(), delta_event)
    };

    // уведомления о новых матчах
    if !new_ids.is_empty() {
        if let Some(lm) = &snap.last_match {
            let _ = app.emit_to("stats", "match-finished", lm);
            notify_match(app, lm);
        }
    }
    if let Some(ev) = elo_delta_event {
        let _ = app.emit_to("stats", "match-elo", ev);
    }
    emit(app, &snap);
    update_discord(&tracker, &discord_id);
}

/// Победы/поражения и ±ELO с локальной полуночи
fn compute_today(log: &[EloPoint], recent: &[RecentMatch], elo: Option<i64>) -> Today {
    let midnight = Local::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|d| Local.from_local_datetime(&d).single())
        .map(|d| d.timestamp_millis())
        .unwrap_or(0);
    let mut t = Today::default();
    for m in recent.iter().filter(|m| m.finished_at >= midnight) {
        match m.win {
            Some(true) => t.wins += 1,
            Some(false) => t.losses += 1,
            None => {}
        }
    }
    // ELO на начало дня: последняя точка до полуночи, иначе первая за сегодня
    let start = log
        .iter()
        .rev()
        .find(|p| p.t < midnight)
        .or_else(|| log.iter().find(|p| p.t >= midnight))
        .map(|p| p.elo);
    if let (Some(s), Some(e)) = (start, elo) {
        t.delta = e - s;
    }
    t
}

/// Итоги матча для игрока: K/D, ADR, место в команде и т.д.
async fn match_summary(app: &AppHandle, key: &str, mid: &str, pid: &str) -> Option<Value> {
    let st = app.state::<AppState>();
    let stats = st.api.get(key, &format!("/matches/{mid}/stats"), &[]).await.ok()?;
    let round = stats["rounds"].get(0)?;
    let teams = round["teams"].as_array()?;
    let my_team = teams.iter().find(|t| {
        t["players"]
            .as_array()
            .is_some_and(|ps| ps.iter().any(|p| p["player_id"].as_str() == Some(pid)))
    })?;
    let players = my_team["players"].as_array()?;
    let me = players.iter().find(|p| p["player_id"].as_str() == Some(pid))?;
    let ps = &me["player_stats"];
    let n = |k: &str| ps[k].as_str().and_then(|v| v.replace(',', ".").parse::<f64>().ok());
    let kills_of = |p: &Value| {
        p["player_stats"]["Kills"].as_str().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0)
    };
    let my_kills = kills_of(me);
    let rank = 1 + players.iter().filter(|p| kills_of(p) > my_kills).count();
    let win = my_team["team_id"].as_str().is_some()
        && round["round_stats"]["Winner"].as_str() == my_team["team_id"].as_str();

    Some(json!({
        "match_id": mid,
        "map": round["round_stats"]["Map"].as_str().unwrap_or("?").trim_start_matches("de_"),
        "score": round["round_stats"]["Score"],
        "win": win,
        "kills": n("Kills"),
        "deaths": n("Deaths"),
        "assists": n("Assists"),
        "kd": n("K/D Ratio"),
        "adr": n("ADR"),
        "hs": n("Headshots %"),
        "mvps": n("MVPs"),
        "rank": rank,
        "team_size": players.len(),
        "at": now_ms(),
        "elo_delta": Value::Null,
    }))
}

fn notify_match(app: &AppHandle, m: &Value) {
    let win = m["win"].as_bool().unwrap_or(false);
    let f = |k: &str| m[k].as_f64().map(|v| format!("{v:.0}")).unwrap_or("?".into());
    let body = format!(
        "{} {} · {}/{} · K/D {} · ADR {} · #{} в команде",
        m["map"].as_str().unwrap_or("?"),
        m["score"].as_str().unwrap_or(""),
        f("kills"),
        f("deaths"),
        m["kd"].as_f64().map(|v| format!("{v:.2}")).unwrap_or("?".into()),
        f("adr"),
        m["rank"],
    );
    let _ = app
        .notification()
        .builder()
        .title(if win { "Победа 🟢" } else { "Поражение 🔴" })
        .body(body)
        .show();
}

// ───────── Discord ─────────

fn update_discord(tracker: &Tracker, client_id: &str) {
    let mut g = tracker.inner.lock().unwrap();
    if client_id.is_empty() {
        if let Some(mut c) = g.discord.take() {
            let _ = c.close();
        }
        g.discord_id.clear();
        return;
    }
    if g.discord_id != client_id {
        if let Some(mut c) = g.discord.take() {
            let _ = c.close();
        }
        g.discord_id = client_id.to_string();
        g.discord_last.clear();
    }
    let s = &g.snap;
    let details = match (s.elo, s.level) {
        (Some(e), Some(l)) => format!("{e} ELO · {l} lvl"),
        (Some(e), None) => format!("{e} ELO"),
        _ => "FACEIT".into(),
    };
    let state = match &s.room {
        Some(r) => match &r.map {
            Some(m) => format!("В матче · {m}"),
            None => "В комнате матча".into(),
        },
        None if s.today.wins + s.today.losses > 0 => format!(
            "Сегодня {}W {}L · {:+} ELO",
            s.today.wins, s.today.losses, s.today.delta
        ),
        None => "В меню".into(),
    };
    let key = format!("{details}|{state}");
    if key == g.discord_last && g.discord.is_some() {
        return;
    }
    if g.discord.is_none() {
        let mut c = DiscordIpcClient::new(client_id);
        if c.connect().is_err() {
            return; // Discord не запущен — попробуем на следующем тике
        }
        g.discord = Some(c);
    }
    let act = activity::Activity::new().details(details).state(state);
    let ok = g.discord.as_mut().is_some_and(|c| c.set_activity(act).is_ok());
    if ok {
        g.discord_last = key;
    } else {
        g.discord = None; // переподключимся
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn did_win_detects_team() {
        let m = json!({
            "results": { "winner": "faction2" },
            "teams": {
                "faction1": { "players": [{ "player_id": "a" }] },
                "faction2": { "players": [{ "player_id": "me" }] }
            }
        });
        assert_eq!(did_win(&m, "me"), Some(true));
        assert_eq!(did_win(&m, "a"), Some(false));
        assert_eq!(did_win(&m, "nobody"), None);
    }

    #[test]
    fn today_counts_since_midnight() {
        let midnight = Local::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .and_then(|d| Local.from_local_datetime(&d).single())
            .unwrap()
            .timestamp_millis();
        let log = vec![
            EloPoint { t: midnight - 3_600_000, elo: 1500, match_id: None }, // вчера
            EloPoint { t: midnight + 60_000, elo: 1525, match_id: None },
        ];
        let rm = |t, win| RecentMatch { match_id: "x".into(), finished_at: t, win: Some(win) };
        let recent = vec![
            rm(midnight + 60_000, true),
            rm(midnight + 120_000, false),
            rm(midnight - 60_000, true), // вчера — не считается
        ];
        let t = compute_today(&log, &recent, Some(1540));
        assert_eq!((t.wins, t.losses, t.delta), (1, 1, 40));
    }
}
