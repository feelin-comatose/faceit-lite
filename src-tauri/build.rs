fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "get_config",
            "save_config",
            "reload_theme",
            "open_config_dir",
            "get_player",
            "get_match",
            "page_event",
            "toggle_site",
            "get_site_visible",
            "check_update",
            "install_update",
            "unload_site",
            "get_tracker",
            "refresh_tracker",
            "get_friends",
            "get_resources",
        ]),
    ))
    .expect("failed to run tauri-build");
}
