#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod config;

use config::{Config, Paths};
use serde_json::{json, Value};
use std::sync::Mutex;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, UserAttentionType, WebviewUrl, WebviewWindowBuilder,
    WindowEvent,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tauri_plugin_notification::NotificationExt;

const INJECT_JS: &str = include_str!("../inject/inject.js");

struct AppState {
    cfg: Mutex<Config>,
    paths: Paths,
    api: api::Faceit,
    hotkeys: Mutex<Hotkeys>,
}

#[derive(Default)]
struct Hotkeys {
    stats: Option<Shortcut>,
    main: Option<Shortcut>,
}

fn cfg(app: &AppHandle) -> Config {
    app.state::<AppState>().cfg.lock().unwrap().clone()
}

/// Данные, которые получает страница faceit.com (без API-ключа)
fn page_payload(app: &AppHandle) -> Value {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap().for_page();
    json!({ "config": c, "css": st.paths.faceit_css() })
}

fn inject_script(app: &AppHandle) -> String {
    format!("window.__FL_INIT__ = {};\n{}", page_payload(app), INJECT_JS)
}

fn show_window(app: &AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn toggle_window(app: &AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        let visible = w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false);
        if visible {
            let _ = w.hide();
        } else {
            show_window(app, label);
        }
    }
}

/// Перечитать/применить тему везде без перезапуска
fn apply_theme(app: &AppHandle) {
    let c = cfg(app);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.eval(&format!("window.__FL_apply && window.__FL_apply({})", page_payload(app)));
        let _ = w.set_zoom(c.zoom.clamp(0.5, 2.0));
    }
    if let Some(w) = app.get_webview_window("stats") {
        let _ = w.set_always_on_top(c.stats_always_on_top);
    }
    let _ = app.emit("config-changed", ());
}

fn on_match_found(app: &AppHandle) {
    let c = cfg(app);
    if c.focus_on_match {
        show_window(app, "main");
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.request_user_attention(Some(UserAttentionType::Critical));
        }
    }
    if c.notify_on_match {
        let _ = app
            .notification()
            .builder()
            .title("МАТЧ НАЙДЕН")
            .body("Беги жать Accept")
            .show();
    }
}

// ───────────── команды ─────────────

#[tauri::command]
fn get_config(state: State<AppState>) -> Value {
    let c = state.cfg.lock().unwrap().clone();
    json!({
        "config": c,
        "panel_css": state.paths.panel_css(),
        "dir": state.paths.dir.display().to_string(),
    })
}

#[tauri::command]
fn save_config(app: AppHandle, state: State<AppState>, config: Config) -> Result<(), String> {
    state.paths.save(&config).map_err(|e| e.to_string())?;
    *state.cfg.lock().unwrap() = config;
    apply_theme(&app);
    Ok(())
}

/// Перечитать config.json и css-файлы с диска (если правил руками)
#[tauri::command]
fn reload_theme(app: AppHandle, state: State<AppState>) {
    let fresh = state.paths.load();
    *state.cfg.lock().unwrap() = fresh;
    apply_theme(&app);
}

#[tauri::command]
fn open_config_dir(state: State<AppState>) -> Result<(), String> {
    let dir = state.paths.dir.clone();
    #[cfg(target_os = "windows")]
    let cmd = "explorer";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = "xdg-open";
    std::process::Command::new(cmd)
        .arg(dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_player(state: State<'_, AppState>, nickname: String) -> Result<Value, String> {
    let key = state.cfg.lock().unwrap().api_key.clone();
    state.api.player_bundle(&key, &nickname).await
}

#[tauri::command]
async fn get_match(state: State<'_, AppState>, match_id: String) -> Result<Value, String> {
    let id = match_id.trim().to_string();
    if !api::valid_match_id(&id) {
        return Err("Кривой ID матча".into());
    }
    let key = state.cfg.lock().unwrap().api_key.clone();
    state.api.match_bundle(&key, &id).await
}

/// Единственная команда, доступная сайту faceit.com
#[tauri::command]
fn page_event(app: AppHandle, kind: String, data: Option<String>) -> Value {
    match kind.as_str() {
        "ready" => page_payload(&app),
        "match_found" => {
            on_match_found(&app);
            Value::Null
        }
        "room" => {
            if let Some(id) = data.filter(|d| api::valid_match_id(d)) {
                let _ = app.emit_to("stats", "room-detected", id);
                if cfg(&app).auto_open_lobby {
                    if let Some(w) = app.get_webview_window("stats") {
                        let _ = w.show();
                    }
                }
            }
            Value::Null
        }
        _ => Value::Null,
    }
}

// ───────────── setup ─────────────

fn build_windows(app: &AppHandle) -> tauri::Result<()> {
    let c = cfg(app);

    let start: tauri::Url = c
        .start_url
        .parse()
        .ok()
        .filter(|u: &tauri::Url| u.host_str().is_some_and(|h| h == "faceit.com" || h.ends_with(".faceit.com")))
        .unwrap_or_else(|| "https://www.faceit.com/".parse().unwrap());

    #[allow(unused_mut)]
    let mut main = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(start))
        .title("FACEIT Lite")
        .inner_size(1280.0, 800.0)
        .min_inner_size(900.0, 600.0)
        .visible(!c.start_hidden)
        .devtools(c.devtools)
        .initialization_script(&inject_script(app));
    #[cfg(target_os = "windows")]
    if !c.webview_args.trim().is_empty() {
        main = main.additional_browser_args(c.webview_args.trim());
    }
    let main = main.build()?;
    let _ = main.set_zoom(c.zoom.clamp(0.5, 2.0));

    WebviewWindowBuilder::new(app, "stats", WebviewUrl::App("index.html".into()))
        .title("FACEIT Lite — статистика")
        .inner_size(460.0, 760.0)
        .min_inner_size(360.0, 400.0)
        .always_on_top(c.stats_always_on_top)
        .devtools(c.devtools)
        .visible(false)
        .build()?;

    Ok(())
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Открыть FACEIT", true, None::<&str>)?;
    let stats = MenuItem::with_id(app, "stats", "Статистика / лобби", true, None::<&str>)?;
    let reload = MenuItem::with_id(app, "reload", "Перечитать тему и конфиг", true, None::<&str>)?;
    let dir = MenuItem::with_id(app, "dir", "Папка настроек", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &stats, &sep1, &reload, &dir, &sep2, &quit])?;

    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("FACEIT Lite")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id.as_ref() {
            "show" => show_window(app, "main"),
            "stats" => show_window(app, "stats"),
            "reload" => reload_theme(app.clone(), app.state()),
            "dir" => {
                let _ = open_config_dir(app.state());
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
            {
                toggle_window(tray.app_handle(), "main");
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn register_hotkeys(app: &AppHandle) {
    let c = cfg(app);
    let st = app.state::<AppState>();
    let reg = |s: &str| -> Option<Shortcut> {
        if s.trim().is_empty() {
            return None;
        }
        match s.parse::<Shortcut>() {
            Ok(sc) => match app.global_shortcut().register(sc) {
                Ok(()) => Some(sc),
                Err(e) => {
                    eprintln!("hotkey {s} not registered: {e}");
                    None
                }
            },
            Err(e) => {
                eprintln!("bad hotkey {s}: {e}");
                None
            }
        }
    };
    let stats = reg(&c.hotkey_toggle_stats);
    let main = reg(&c.hotkey_toggle_main);
    *st.hotkeys.lock().unwrap() = Hotkeys { stats, main };
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    let (is_stats, is_main) = {
                        let st = app.state::<AppState>();
                        let hk = st.hotkeys.lock().unwrap();
                        (hk.stats.as_ref() == Some(shortcut), hk.main.as_ref() == Some(shortcut))
                    };
                    if is_stats {
                        toggle_window(app, "stats");
                    } else if is_main {
                        toggle_window(app, "main");
                    }
                })
                .build(),
        )
        .setup(|app| {
            let paths = Paths::new(app.path().app_config_dir()?);
            let cfg = paths.load();
            app.manage(AppState {
                cfg: Mutex::new(cfg),
                paths,
                api: api::Faceit::new(),
                hotkeys: Mutex::new(Hotkeys::default()),
            });
            let handle = app.handle().clone();
            build_windows(&handle)?;
            build_tray(&handle)?;
            register_hotkeys(&handle);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                // Окно статистики всегда просто прячется; главное — по настройке
                if window.label() == "stats" || cfg(app).close_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                } else {
                    app.exit(0);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            reload_theme,
            open_config_dir,
            get_player,
            get_match,
            page_event
        ])
        .run(tauri::generate_context!())
        .expect("error while running FACEIT Lite");
}
