//! Local status server: `GET http://127.0.0.1:<port>/status` reports the running build,
//! the provider usage snapshots and the detected activity, for diagnosing the installed
//! instance. It excludes conversation content and credentials.

use crate::AppState;
use tauri::{AppHandle, Manager};

pub fn start(app: AppHandle, port: u16) {
    std::thread::spawn(move || {
        let server = match tiny_http::Server::http(("127.0.0.1", port)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[codenotch] failed to bind port {port}: {e} (is another instance running?)");
                return;
            }
        };
        for req in server.incoming_requests() {
            let url = req.url().to_string();
            if url == "/status" && req.method() == &tiny_http::Method::Get {
                let state = app.state::<AppState>();
                let body = serde_json::json!({"build": crate::BUILD,
                    "activity":state.activity.lock().unwrap().iter().map(|a|
                        serde_json::json!({"provider":a.provider,"state":a.state,"since":a.since})).collect::<Vec<_>>(),
                    "codex":state.codex.lock().unwrap().clone(),
                    "kimi":state.kimi.lock().unwrap().clone()}).to_string();
                let _ = req.respond(tiny_http::Response::from_string(body));
                continue;
            }
            let _ = req.respond(tiny_http::Response::from_string("ok"));
        }
    });
}
