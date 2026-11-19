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
        match status {
            200..=299 => resp.json().await.map_err(|e| format!("Кривой ответ API: {e}")),
            401 | 403 => Err("API-ключ не подошёл (401/403). Нужен Server-side ключ.".into()),
            404 => Err("Не найдено (404). Проверь ник / ID матча.".into()),
            429 => Err("FACEIT режет запросы (429). Подожди немного.".into()),
            _ => Err(format!("FACEIT API ответил HTTP {status}")),
        }
    }

    /// Профиль + общая статистика + последние матчи
    pub async fn player_bundle(&self, key: &str, nickname: &str) -> Result<Value, String> {
        let player = self
            .get(key, "/players", &[("nickname", nickname.trim()), ("game", "cs2")])
            .await?;
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
