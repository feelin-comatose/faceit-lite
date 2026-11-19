//! Локальный сервер для OBS Browser Source: http://127.0.0.1:<port>/
//! Слушает только localhost, без внешних зависимостей.

use crate::{tracker::Tracker, AppState};
use std::{
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    time::Duration,
};
use tauri::{AppHandle, Manager};

const OVERLAY_HTML: &str = include_str!("../overlay/overlay.html");

pub fn start(app: AppHandle, port: u16) {
    std::thread::spawn(move || {
        let listener = match TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("overlay: cannot bind 127.0.0.1:{port}: {e}");
                return;
            }
        };
        for stream in listener.incoming().flatten() {
            let app = app.clone();
            std::thread::spawn(move || {
                let _ = handle(&app, stream);
            });
        }
    });
}

fn handle(app: &AppHandle, mut stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    // "GET /path?query HTTP/1.1"
    let path = line.split_whitespace().nth(1).unwrap_or("/");
    let path = path.split('?').next().unwrap_or("/");

    let (status, ctype, body) = match path {
        "/" | "/overlay" => ("200 OK", "text/html; charset=utf-8", OVERLAY_HTML.to_string()),
        "/state" => {
            let snap = app.state::<Tracker>().snapshot();
            let theme = app.state::<AppState>().cfg.lock().unwrap().theme.clone();
            let body = serde_json::json!({ "snap": snap, "theme": theme }).to_string();
            ("200 OK", "application/json; charset=utf-8", body)
        }
        "/user.css" => (
            "200 OK",
            "text/css; charset=utf-8",
            app.state::<AppState>().paths.overlay_css(),
        ),
        _ => ("404 Not Found", "text/plain", "not found".into()),
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body.as_bytes())
}
