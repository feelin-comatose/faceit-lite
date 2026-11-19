#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod config;
mod overlay;
mod tracker;

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
use tauri_plugin_updater::{Update, UpdaterExt};

const INJECT_JS: &str = include_str!("../inject/inject.js");

struct AppState {
    cfg: Mutex<Config>,
    paths: Paths,
    api: api::Faceit,
    hotkeys: Mutex<Hotkeys>,
}

/// Найденное, но ещё не установленное обновление
struct PendingUpdate(Mutex<Option<Update>>);

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

/// Сайт спрятан → WebView2 считает себя невидимым (не рисует, троттлит таймеры)
/// и держит минимум памяти. JS при этом работает — алерт о матче не ломается.
#[cfg(target_os = "windows")]
fn set_site_active(app: &AppHandle, active: bool) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.with_webview(move |wv| unsafe {
            use webview2_com::Microsoft::Web::WebView2::Win32::{
                ICoreWebView2_19, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW,
                COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
            };
            use windows_core::Interface;
            let controller = wv.controller();
            let _ = controller.SetIsVisible(active);
            if let Ok(core) = controller.CoreWebView2() {
                if let Ok(core19) = core.cast::<ICoreWebView2_19>() {
                    let level = if active {
                        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
                    } else {
                        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
                    };
                    let _ = core19.SetMemoryUsageTargetLevel(level);
                }
            }
        });
    }
}

#[cfg(not(target_os = "windows"))]
fn set_site_active(_app: &AppHandle, _active: bool) {}

fn site_visible(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .map(|w| w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false))
        .unwrap_or(false)
}

fn notify_site_status(app: &AppHandle) {
    let loaded = app.get_webview_window("main").is_some();
    let _ = app.emit_to(
        "stats",
        "site-status",
        json!({ "kind": "visibility", "visible": site_visible(app), "loaded": loaded }),
    );
}

fn show_window(app: &AppHandle, label: &str) {
    // сайт был выгружен — создаём заново
    if label == "main" && app.get_webview_window("main").is_none() {
        if let Err(e) = build_main(app, true) {
            eprintln!("cannot rebuild main window: {e}");
        }
        notify_site_status(app);
        return;
    }
    if let Some(w) = app.get_webview_window(label) {
        if label == "main" {
            set_site_active(app, true);
        }
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        if label == "main" {
            notify_site_status(app);
        }
    }
}

fn hide_window(app: &AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        let _ = w.hide();
        if label == "main" {
            set_site_active(app, false);
            notify_site_status(app);
        }
    }
}

fn toggle_window(app: &AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        let visible = w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false);
        if visible {
            hide_window(app, label);
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
    let _ = app.emit_to("stats", "site-status", json!({ "kind": "match_found" }));
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
fn get_config(app: AppHandle, state: State<AppState>) -> Value {
    let c = state.cfg.lock().unwrap().clone();
    json!({
        "version": app.package_info().version.to_string(),
        "config": c,
        "panel_css": state.paths.panel_css(),
        "dir": state.paths.dir.display().to_string(),
    })
}

/// Показать/спрятать сайт из панели. Возвращает новую видимость.
#[tauri::command]
fn toggle_site(app: AppHandle) -> bool {
    toggle_window(&app, "main");
    site_visible(&app)
}

#[tauri::command]
fn get_site_visible(app: AppHandle) -> Value {
    json!({ "visible": site_visible(&app), "loaded": app.get_webview_window("main").is_some() })
}

/// Полностью выгрузить сайт из памяти. Алерт «матч найден» до открытия работать не будет.
#[tauri::command]
fn unload_site(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.destroy();
    }
    notify_site_status(&app);
}

#[tauri::command]
fn get_tracker(tracker: State<tracker::Tracker>) -> tracker::Snapshot {
    tracker.snapshot()
}

#[tauri::command]
fn refresh_tracker(tracker: State<tracker::Tracker>) {
    tracker.wake();
}

#[tauri::command]
async fn get_friends(state: State<'_, AppState>) -> Result<Vec<Value>, String> {
    let (key, friends) = {
        let c = state.cfg.lock().unwrap();
        (c.api_key.clone(), c.friends.clone())
    };
    Ok(state.api.friends(&key, &friends).await)
}

/// Сколько памяти занимает клиент вместе со всеми процессами WebView2
#[tauri::command]
fn get_resources() -> Value {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let Ok(mut sys) = System::new() else {
        return json!({ "error": "не удалось прочитать список процессов" });
    };
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let me = Pid::from_u32(std::process::id());
    let mut tree = std::collections::HashSet::from([me]);
    loop {
        let before = tree.len();
        for (pid, p) in sys.processes() {
            if p.parent().is_some_and(|pp| tree.contains(&pp)) {
                tree.insert(*pid);
            }
        }
        if tree.len() == before {
            break;
        }
    }
    let bytes: u64 = tree.iter().filter_map(|p| sys.process(*p)).map(|p| p.memory()).sum();
    json!({ "ram_mb": bytes / 1024 / 1024, "processes": tree.len() })
}

#[tauri::command]
fn save_config(app: AppHandle, state: State<AppState>, config: Config) -> Result<(), String> {
    state.paths.save(&config).map_err(|e| e.to_string())?;
    *state.cfg.lock().unwrap() = config;
    apply_theme(&app);
    app.state::<tracker::Tracker>().wake();
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

// ───────── автообновление ─────────

/// Проверяет latest.json в GitHub Releases. notify=true — ещё и системное уведомление.
#[tauri::command]
async fn check_update(app: AppHandle, notify: bool) -> Result<Value, String> {
    let current = app.package_info().version.to_string();
    let update = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| format!("Не удалось проверить обновления: {e}"))?;
    let Some(u) = update else {
        return Ok(json!({ "available": false, "current": current }));
    };
    let info = json!({
        "available": true,
        "current": current,
        "version": u.version,
        "notes": u.body,
    });
    if notify {
        let _ = app
            .notification()
            .builder()
            .title("FACEIT Lite: есть обновление")
            .body(format!("Версия {} — нажми «Обновить» в панели", u.version))
            .show();
    }
    *app.state::<PendingUpdate>().0.lock().unwrap() = Some(u);
    Ok(info)
}

/// Скачивает, проверяет подпись, ставит и перезапускает клиент
#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    let update = app
        .state::<PendingUpdate>()
        .0
        .lock()
        .unwrap()
        .take()
        .ok_or("Сначала проверь обновления")?;
    let progress_app = app.clone();
    let mut got: usize = 0;
    update
        .download_and_install(
            move |chunk, total| {
                got += chunk;
                let _ = progress_app.emit_to("stats", "update-progress", json!({ "got": got, "total": total }));
            },
            || {},
        )
        .await
        .map_err(|e| format!("Обновление не встало: {e}"))?;
    app.restart();
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
                app.state::<tracker::Tracker>().set_room(id.clone());
                let _ = app.emit_to("stats", "room-detected", id.clone());
                let _ = app.emit_to("stats", "site-status", json!({ "kind": "room", "id": id }));
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

fn build_main(app: &AppHandle, visible: bool) -> tauri::Result<()> {
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
        .visible(visible)
        .devtools(c.devtools)
        .initialization_script(&inject_script(app));
    #[cfg(target_os = "windows")]
    if !c.webview_args.trim().is_empty() {
        main = main.additional_browser_args(c.webview_args.trim());
    }
    let main = main.build()?;
    let _ = main.set_zoom(c.zoom.clamp(0.5, 2.0));
    if !visible {
        set_site_active(app, false);
    }
    Ok(())
}

fn build_windows(app: &AppHandle) -> tauri::Result<()> {
    let c = cfg(app);
    build_main(app, !c.start_hidden && !c.tray_mode)?;

    WebviewWindowBuilder::new(app, "stats", WebviewUrl::App("index.html".into()))
        .title("FACEIT Lite — статистика")
        .inner_size(480.0, 800.0)
        .min_inner_size(360.0, 400.0)
        .always_on_top(c.stats_always_on_top)
        .devtools(c.devtools)
        .visible(c.tray_mode && !c.start_hidden)
        .build()?;

    Ok(())
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Открыть FACEIT", true, None::<&str>)?;
    let stats = MenuItem::with_id(app, "stats", "Статистика / лобби", true, None::<&str>)?;
    let reload = MenuItem::with_id(app, "reload", "Перечитать тему и конфиг", true, None::<&str>)?;
    let dir = MenuItem::with_id(app, "dir", "Папка настроек", true, None::<&str>)?;
    let upd = MenuItem::with_id(app, "update", "Проверить обновления", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &stats, &sep1, &reload, &dir, &upd, &sep2, &quit])?;

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
            "update" => {
                show_window(app, "stats");
                let _ = app.emit_to("stats", "check-update", ());
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
                let app = tray.app_handle();
                let label = if cfg(app).tray_mode { "stats" } else { "main" };
                toggle_window(app, label);
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
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(PendingUpdate(Mutex::new(None)))
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
            let dir = app.path().app_config_dir()?;
            let paths = Paths::new(dir.clone());
            let cfg = paths.load();
            app.manage(tracker::Tracker::new(dir));
            let (overlay_on, overlay_port) = (cfg.overlay_enabled, cfg.overlay_port);
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
            tracker::start(handle.clone());
            if overlay_on {
                overlay::start(handle.clone(), overlay_port);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                // Окно статистики всегда просто прячется; главное — по настройке
                if window.label() == "stats" || cfg(app).close_to_tray {
                    api.prevent_close();
                    hide_window(app, window.label());
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
            page_event,
            toggle_site,
            get_site_visible,
            check_update,
            install_update,
            unload_site,
            get_tracker,
            refresh_tracker,
            get_friends,
            get_resources
        ])
        .run(tauri::generate_context!())
        .expect("error while running FACEIT Lite");
}
