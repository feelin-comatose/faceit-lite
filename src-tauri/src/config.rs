use serde::{Deserialize, Serialize};
use std::{fs, io, path::PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Theme {
    pub accent: String,
    pub bg: String,
    pub panel: String,
    pub text: String,
    pub muted: String,
    pub good: String,
    pub bad: String,
    pub font: String,
    pub radius: u32,
    /// Прозрачность окна статистики (0.3 – 1.0)
    pub panel_opacity: f64,
    /// Фон самого faceit.com (пусто = не трогать)
    pub page_bg: String,
    /// Шрифт на faceit.com (пусто = не трогать)
    pub page_font: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            accent: "#ff5a1f".into(),
            bg: "#0f1013".into(),
            panel: "#181a1f".into(),
            text: "#e8e8ea".into(),
            muted: "#8b8f98".into(),
            good: "#3ddc84".into(),
            bad: "#ff4d5e".into(),
            font: "Inter, 'Segoe UI', system-ui, sans-serif".into(),
            radius: 8,
            panel_opacity: 1.0,
            page_bg: String::new(),
            page_font: String::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    /// Ключ FACEIT Data API (developers.faceit.com → App Studio → API Keys → Server-side)
    pub api_key: String,
    /// Твой ник — используется по умолчанию и чтобы понять «свою» команду в лобби
    pub nickname: String,
    pub start_url: String,
    pub theme: Theme,
    /// CSS-селекторы на faceit.com, которые надо скрыть
    pub hide_selectors: Vec<String>,
    /// Режим экономии: без анимаций, без видео
    pub low_power: bool,
    pub zoom: f64,
    pub notify_on_match: bool,
    pub focus_on_match: bool,
    pub sound_on_match: bool,
    /// Регулярка (JS) для текста кнопки принятия матча
    pub match_found_regex: String,
    /// Автоматически открывать разбор лобби, когда заходишь в комнату матча
    pub auto_open_lobby: bool,
    pub stats_always_on_top: bool,
    pub close_to_tray: bool,
    /// Режим «сайт в трее»: faceit.com спрятан и почти не рендерится,
    /// главное окно — панель; сайт всплывает только на «матч найден»
    pub tray_mode: bool,
    pub start_hidden: bool,
    pub hotkey_toggle_stats: String,
    pub hotkey_toggle_main: String,
    /// Разрешить DevTools (ПКМ → Inspect) — удобно, чтобы писать свой CSS
    pub devtools: bool,
    /// Доп. аргументы для WebView2 (только Windows), напр. "--disable-gpu"
    pub webview_args: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            nickname: String::new(),
            start_url: "https://www.faceit.com/ru/home".into(),
            theme: Theme::default(),
            hide_selectors: vec![],
            low_power: true,
            zoom: 1.0,
            notify_on_match: true,
            focus_on_match: true,
            sound_on_match: true,
            match_found_regex: "^(accept|принять)$".into(),
            auto_open_lobby: true,
            stats_always_on_top: false,
            close_to_tray: true,
            tray_mode: true,
            start_hidden: false,
            hotkey_toggle_stats: "CommandOrControl+Shift+F".into(),
            hotkey_toggle_main: "CommandOrControl+Shift+G".into(),
            devtools: false,
            webview_args: String::new(),
        }
    }
}

impl Config {
    /// Версия конфига, которую можно безопасно отдать странице faceit.com (без API-ключа)
    pub fn for_page(&self) -> Config {
        Config {
            api_key: String::new(),
            ..self.clone()
        }
    }
}

const FACEIT_CSS_TEMPLATE: &str = r#"/*
  faceit.css — твой CSS поверх faceit.com. Применяется сразу (трей → «Перечитать тему»).

  У FACEIT классы хэшированные (styled-components), они меняются после апдейтов,
  поэтому цепляйся за стабильные штуки: [data-testid], [aria-label], теги, [class*="Кусок"].
  Включи "devtools": true в config.json → ПКМ → Inspect, чтобы искать элементы.

  CSS-переменные из темы доступны: var(--fl-accent), var(--fl-bg), var(--fl-text) ...

  Примеры:
  html { font-size: 14px; }                        — всё компактнее
  img[src*="banner"] { display: none !important; } — убрать баннеры
  ::-webkit-scrollbar { width: 6px; }
  ::-webkit-scrollbar-thumb { background: var(--fl-accent); border-radius: 3px; }
*/
"#;

const PANEL_CSS_TEMPLATE: &str = r#"/*
  panel.css — твой CSS для окна статистики. Тут классы наши и стабильные:
  .card, .player, .lvl, .elo, .match-row.win / .match-row.loss, .map-row, .team, .tabs ...

  Пример:
  .match-row.win  { border-left: 3px solid var(--good); }
  .match-row.loss { border-left: 3px solid var(--bad); }
*/
"#;

pub struct Paths {
    pub dir: PathBuf,
}

impl Paths {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }
    fn config_file(&self) -> PathBuf {
        self.dir.join("config.json")
    }
    fn faceit_css_file(&self) -> PathBuf {
        self.dir.join("faceit.css")
    }
    fn panel_css_file(&self) -> PathBuf {
        self.dir.join("panel.css")
    }

    /// Загружает конфиг; при первом запуске создаёт файлы-шаблоны.
    pub fn load(&self) -> Config {
        let _ = fs::create_dir_all(&self.dir);
        for (path, tpl) in [
            (self.faceit_css_file(), FACEIT_CSS_TEMPLATE),
            (self.panel_css_file(), PANEL_CSS_TEMPLATE),
        ] {
            if !path.exists() {
                let _ = fs::write(path, tpl);
            }
        }
        let file = self.config_file();
        match fs::read_to_string(&file) {
            Ok(text) => match serde_json::from_str::<Config>(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    // Сломанный JSON не затираем молча — сохраняем копию
                    eprintln!("config.json parse error: {e}");
                    let _ = fs::copy(&file, self.dir.join("config.broken.json"));
                    Config::default()
                }
            },
            Err(_) => {
                let cfg = Config::default();
                let _ = self.save(&cfg);
                cfg
            }
        }
    }

    pub fn save(&self, cfg: &Config) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let json = serde_json::to_string_pretty(cfg).map_err(io::Error::other)?;
        fs::write(self.config_file(), json)
    }

    pub fn faceit_css(&self) -> String {
        fs::read_to_string(self.faceit_css_file()).unwrap_or_default()
    }

    pub fn panel_css(&self) -> String {
        fs::read_to_string(self.panel_css_file()).unwrap_or_default()
    }
}
