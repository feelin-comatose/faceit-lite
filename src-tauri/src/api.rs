//! Тонкий клиент к официальному FACEIT Data API v4.
//! Запросы идут из Rust — ключ не светится в веб-страницах, нет CORS-проблем.

use futures::future::join_all;
use serde_json::{json, Map, Value};
use std::time::Duration;

const BASE: &str = "https://open.faceit.com/data/v4";

pub struct Faceit {
    http: reqwest::Client,
}

impl Faceit {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent("faceit-lite/0.1")
                .timeout(Duration::from_secs(15))
                .build()
                .expect("http client"),
        }
    }

    async fn get(&self, key: &str, path: &str, query: &[(&str, &str)]) -> Result<Value, String> {
        let key = key.trim();
        if key.is_empty() {
            return Err("Нет API-ключа. Вставь его во вкладке «Настройки» (developers.faceit.com).".into());
        }
        let resp = self
            .http
            .get(format!("{BASE}{path}"))
            .bearer_auth(key)
            .query(query)
            .send()
            .await
            .map_err(|e| format!("Сеть: {e}"))?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return resp.json().await.map_err(|e| format!("Кривой ответ API: {e}"));
        }
        // FACEIT кладёт причину в {"errors":[{"message":"..."}]} — показываем её
        let body = resp.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v["errors"][0]["message"].as_str().map(String::from))
            .or_else(|| {
                // нет привычного JSON — покажем кусок сырого ответа (кроме HTML-страниц)
                let t = body.trim();
                (!t.is_empty() && !t.starts_with('<')).then(|| t.chars().take(200).collect())
            })
            .map(|m| format!(" — {m}"))
            .unwrap_or_default();
        Err(match status {
            401 | 403 => format!("API-ключ не подошёл ({status}). Нужен Server-side ключ.{detail}"),
            404 => format!("Не найдено (404). Проверь ник / ID матча.{detail}"),
            429 => "FACEIT режет запросы (429). Подожди немного.".into(),
            _ => format!("FACEIT API ответил HTTP {status}{detail}"),
        })
    }

    /// Ник в FACEIT чувствителен к регистру: сначала точный поиск,
    /// при 404 — через /search/players с совпадением без учёта регистра.
    async fn find_player(&self, key: &str, nickname: &str) -> Result<Value, String> {
        let nick = nickname.trim();
        match self.get(key, "/players", &[("nickname", nick)]).await {
            Ok(p) => return Ok(p),
            Err(e) if !e.starts_with("Не найдено") => return Err(e),
            Err(_) => {}
        }
        let found = self
            .get(key, "/search/players", &[("nickname", nick), ("game", "cs2"), ("limit", "20")])
            .await?;
        let id = found["items"]
            .as_array()
            .and_then(|items| {
                items.iter().find(|p| {
                    p["nickname"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(nick))
                })
            })
            .and_then(|p| p["player_id"].as_str().map(String::from))
            .ok_or_else(|| format!("Игрок «{nick}» не найден на FACEIT"))?;
        self.get(key, &format!("/players/{id}"), &[]).await
    }

    /// Профиль + общая статистика + последние матчи
    pub async fn player_bundle(&self, key: &str, nickname: &str) -> Result<Value, String> {
        let player = self.find_player(key, nickname).await?;
        let id = player["player_id"]
            .as_str()
            .ok_or("В ответе нет player_id")?
            .to_string();
        let stats_path = format!("/players/{id}/stats/cs2");
        let recent_path = format!("/players/{id}/games/cs2/stats");
        let (stats, recent) = futures::join!(
            self.get(key, &stats_path, &[]),
            self.get(key, &recent_path, &[("limit", "30")]),
        );
        Ok(json!({
            "player": player,
            "stats": stats.unwrap_or(Value::Null),
            "recent": recent.unwrap_or(Value::Null),
        }))
    }

    /// Матч + профиль и статистика каждого игрока в лобби (для разбора карт)
    pub async fn match_bundle(&self, key: &str, match_id: &str) -> Result<Value, String> {
        let m = self.get(key, &format!("/matches/{match_id}"), &[]).await?;
        let ids: Vec<String> = ["faction1", "faction2"]
            .iter()
            .flat_map(|f| m["teams"][f]["roster"].as_array().cloned().unwrap_or_default())
            .filter_map(|p| p["player_id"].as_str().map(String::from))
            .collect();

        let futs = ids.iter().map(|pid| async move {
            let p_path = format!("/players/{pid}");
            let s_path = format!("/players/{pid}/stats/cs2");
            let (p, s) = futures::join!(self.get(key, &p_path, &[]), self.get(key, &s_path, &[]));
            (
                pid.clone(),
                json!({ "player": p.unwrap_or(Value::Null), "stats": s.unwrap_or(Value::Null) }),
            )
        });
        let players: Map<String, Value> = join_all(futs).await.into_iter().collect();
        Ok(json!({ "match": m, "players": players }))
    }
}

/// ID матча FACEIT: "1-<uuid>" — пропускаем только безопасные символы
pub fn valid_match_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}
