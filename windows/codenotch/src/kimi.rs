//! Kimi Code provider.
//!
//! Source: the Kimi Code local server the desktop app / `kimi web` runs. It proxies the
//! membership quota endpoint and answers with the same envelope the CLI's `/usage` shows:
//!
//!   GET  http://127.0.0.1:<port>/api/v1/oauth/usage
//!   Authorization: Bearer <~/.kimi-code/server.token>
//!   → { code, msg, data: { kind: "ok", quota: { usages: {
//!         limit5h:    { usedRatio, resetAt },
//!         monthTotal: { usedRatio, resetAt },
//!         monthCode:  { usedRatio, resetAt } },
//!       extraUsage: null | {…} } } }
//!
//! The port is not fixed: each server run writes an instance file
//! ~/.kimi-code/server/instances/<server_id>.json { host, port, pid, heartbeat_at } and heartbeats
//! while it lives. The freshest live instance wins; none (or a heartbeat gone quiet) means Kimi
//! Code is not running, which is the "absent" state — no cell, exactly like a provider that is not
//! installed. The reading itself is only ever read: the token goes to 127.0.0.1 and is never
//! logged or written anywhere.
//!
//! Failure discipline matches the other adapters: 401 → needsAuth, 429 → exponential backoff with
//! the deadline persisted, anything else → keep the last reading marked stale rather than invent a
//! number. A dead server is different from a failed request, though: it demotes the cell to
//! "absent" (the server comes back on its own port when Kimi Code restarts).

use crate::AppState;
use crate::usage::{LimitWindow, UsageSnapshot};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const POLL_SECS: u64 = 60;
const BACKOFF_BASE_SECS: u64 = 60;
const BACKOFF_CAP_SECS: u64 = 900;
/// An instance whose heartbeat is older than this is a crashed or killed server
const HEARTBEAT_STALE_MS: u64 = 120_000;
const REQUEST_TIMEOUT_SECS: u64 = 10;

static REFRESH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Immediate refresh from the tray or a command
pub fn request_refresh() {
    REFRESH.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn sleep_interruptible(total_secs: u64) {
    for _ in 0..total_secs {
        if REFRESH.swap(false, std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn kimi_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".kimi-code"))
}

fn instances_dir() -> Option<std::path::PathBuf> {
    kimi_dir().map(|d| d.join("server").join("instances"))
}

fn token_path() -> Option<std::path::PathBuf> {
    kimi_dir().map(|d| d.join("server.token"))
}

/// The reading the app keeps between launches
fn store_path() -> std::path::PathBuf {
    crate::config::config_path().with_file_name("usage-kimi.json")
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Instance {
    host: String,
    port: u16,
    #[serde(default)]
    pid: u32,
    #[serde(default)]
    heartbeat_at: u64,
}

/// The freshest instance that is still heartbeating: (host, port). A stale heartbeat means the
/// server died without removing its file (a crash, a kill) — it must not be asked.
fn live_instance() -> Option<(String, u16)> {
    let dir = instances_dir()?;
    let now = now_ms();
    let mut best: Option<Instance> = None;
    for e in std::fs::read_dir(&dir).ok()?.flatten() {
        let Ok(text) = std::fs::read_to_string(e.path()) else { continue };
        let Ok(inst) = serde_json::from_str::<Instance>(&text) else { continue };
        if now.saturating_sub(inst.heartbeat_at) > HEARTBEAT_STALE_MS {
            continue;
        }
        if inst.host.is_empty() || inst.port == 0 {
            continue;
        }
        if best.as_ref().map(|b| inst.heartbeat_at > b.heartbeat_at).unwrap_or(true) {
            best = Some(inst);
        }
    }
    best.map(|b| (b.host, b.port))
}

fn read_token() -> Option<String> {
    let t = std::fs::read_to_string(token_path()?).ok()?;
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_string())
}

pub fn load_persisted() -> UsageSnapshot {
    std::fs::read_to_string(store_path())
        .ok()
        .and_then(|t| serde_json::from_str::<UsageSnapshot>(&t).ok())
        .map(|mut s| {
            if !s.windows.is_empty() {
                s.status = "stale".into(); // an old reading after a restart is labelled as such
            }
            s
        })
        .unwrap_or_default()
}

fn persist(s: &UsageSnapshot) {
    if let Ok(t) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(store_path(), t);
    }
}

/// For doctor / the tray notice: where the server was found, never what the token is
pub fn probe() -> String {
    match live_instance() {
        Some((host, port)) => format!(
            "Kimi: local server {}:{} (token {} chars)",
            host,
            port,
            read_token().map(|t| t.len()).unwrap_or(0)
        ),
        None => "Kimi: no live local server (start the Kimi Code desktop app or `kimi web`)".into(),
    }
}

#[derive(Debug, Clone, Default)]
pub struct Reading {
    pub windows: Vec<LimitWindow>,
    pub note: String,
}

fn parse_reset(v: &serde_json::Value) -> Option<u64> {
    v.as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp_millis().max(0) as u64)
}

fn window_of(v: &serde_json::Value, id: &str, label: &str, out: &mut Vec<LimitWindow>) {
    let Some(w) = v.get(id) else { return };
    let Some(used) = w.get("usedRatio").and_then(|x| x.as_f64()) else { return };
    out.push(LimitWindow {
        id: id.into(),
        label: label.into(),
        used: used.clamp(0.0, 1.0),
        resets_at: w.get("resetAt").and_then(parse_reset),
        ..Default::default()
    });
}

/// The wire reply → the cell. Pure, so the shape is pinned by tests. `data` is the envelope's
/// `data` field. Unknown quota windows are ignored on purpose (the known three cover the plan).
pub fn reading_from(data: &serde_json::Value) -> Result<Reading, String> {
    match data.get("kind").and_then(|x| x.as_str()).unwrap_or("") {
        "ok" => {}
        "error" => {
            return Err(data
                .get("message")
                .and_then(|x| x.as_str())
                .unwrap_or("Kimi reported an error")
                .to_string())
        }
        other => return Err(format!("unexpected kind {other:?}")),
    }
    let usages = data
        .pointer("/quota/usages")
        .ok_or("quota.usages missing")?;
    let mut windows = Vec::new();
    window_of(usages, "limit5h", "5-hour window", &mut windows);
    window_of(usages, "monthCode", "Monthly (code)", &mut windows);
    window_of(usages, "monthTotal", "Monthly (total)", &mut windows);
    if windows.is_empty() {
        return Ok(Reading {
            windows,
            note: "Kimi reported no quota windows for this account".into(),
        });
    }
    // The account may also carry an Extra Usage balance (a paid fallback wallet). Its shape is
    // whatever the console shows; surfaced as a note only when it is an object with a number.
    let mut note = String::new();
    if let Some(extra) = data.pointer("/quota/extraUsage") {
        if let Some(obj) = extra.as_object() {
            for key in ["balance", "amount", "value"] {
                if let Some(n) = obj.get(key).and_then(|x| x.as_f64()) {
                    note = format!("Extra Usage balance: ¥{n:.2}");
                    break;
                }
            }
        }
    }
    Ok(Reading { windows, note })
}

enum FetchErr {
    NeedsAuth,
    RateLimited(u64),
    Other(String),
}

fn fetch_once(host: &str, port: u16, token: &str) -> Result<Reading, FetchErr> {
    let url = format!("http://{host}:{port}/api/v1/oauth/usage");
    let resp = ureq::get(&url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .call();
    match resp {
        Ok(r) => {
            let v: serde_json::Value = r
                .into_json()
                .map_err(|e| FetchErr::Other(format!("parse: {e}")))?;
            // The envelope: { code, msg, data }. code 0 = success; 401xx = the token/user is rejected.
            match v.get("code").and_then(|x| x.as_i64()).unwrap_or(0) {
                0 => {
                    let data = v.get("data").cloned().unwrap_or(serde_json::Value::Null);
                    reading_from(&data).map_err(FetchErr::Other)
                }
                c if (40100..40200).contains(&c) => Err(FetchErr::NeedsAuth),
                c => Err(FetchErr::Other(format!(
                    "HTTP ok, code {c}: {}",
                    v.get("msg").and_then(|x| x.as_str()).unwrap_or("")
                ))),
            }
        }
        Err(ureq::Error::Status(401, _)) | Err(ureq::Error::Status(403, _)) => Err(FetchErr::NeedsAuth),
        Err(ureq::Error::Status(429, r)) => {
            let ra = r.header("retry-after").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
            Err(FetchErr::RateLimited(ra))
        }
        Err(ureq::Error::Status(code, _)) => Err(FetchErr::Other(format!("HTTP {code}"))),
        Err(e) => Err(FetchErr::Other(format!("{e}"))),
    }
}

fn backoff_secs(consecutive: u32, retry_after_floor: u64) -> u64 {
    let exp = BACKOFF_BASE_SECS.saturating_mul(1u64 << consecutive.min(4));
    exp.clamp(BACKOFF_BASE_SECS, BACKOFF_CAP_SECS).max(retry_after_floor)
}

fn set_and_broadcast(app: &AppHandle, mutate: impl FnOnce(&mut UsageSnapshot)) {
    let st = app.state::<AppState>();
    let snap = {
        let mut u = st.kimi.lock().unwrap();
        mutate(&mut u);
        u.clone()
    };
    persist(&snap);
    let _ = app.emit("kimi", &snap);
}

pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        // No live server = no cell at all; a server can start at any moment (the desktop app
        // autostarts), so the absent state is re-checked every few seconds.
        let mut consecutive_429: u32 = 0;
        loop {
            let backoff_until = {
                let st = app.state::<AppState>();
                let u = st.kimi.lock().unwrap();
                u.backoff_until
            };
            let now = now_ms();
            if backoff_until > now {
                sleep_interruptible(((backoff_until - now) / 1000).clamp(1, 30));
                continue;
            }
            let Some((host, port)) = live_instance() else {
                // Announce "absent" once; re-checking every few seconds is enough while Kimi Code is down
                let already = {
                    let st = app.state::<AppState>();
                    let u = st.kimi.lock().unwrap();
                    u.status == "absent"
                };
                if !already {
                    set_and_broadcast(&app, |u| {
                        u.status = "absent".into();
                        u.note = "Kimi Code is not running".into();
                        u.windows.clear();
                        u.backoff_until = 0;
                    });
                }
                sleep_interruptible(5);
                continue;
            };
            let Some(token) = read_token() else {
                set_and_broadcast(&app, |u| {
                    u.status = "needsAuth".into();
                    u.note = "No Kimi Code server token found — sign in again".into();
                });
                sleep_interruptible(POLL_SECS);
                continue;
            };
            match fetch_once(&host, port, &token) {
                Ok(reading) => {
                    consecutive_429 = 0;
                    set_and_broadcast(&app, |u| {
                        if reading.windows.is_empty() {
                            u.status = "error".into();
                        } else {
                            u.status = "ok".into();
                        }
                        u.windows = reading.windows;
                        u.note = reading.note;
                        u.fetched_at = now_ms();
                        u.backoff_until = 0;
                    });
                }
                Err(FetchErr::NeedsAuth) => set_and_broadcast(&app, |u| {
                    u.status = "needsAuth".into();
                    u.note = "Kimi rejected the session — sign in to Kimi Code again".into();
                }),
                Err(FetchErr::RateLimited(ra)) => {
                    consecutive_429 += 1;
                    let wait = backoff_secs(consecutive_429 - 1, ra);
                    set_and_broadcast(&app, |u| {
                        if !u.windows.is_empty() {
                            u.status = "stale".into();
                        }
                        u.note = format!("Rate limited, retrying in {wait}s");
                        u.backoff_until = now_ms() + wait * 1000;
                    });
                }
                Err(FetchErr::Other(msg)) => set_and_broadcast(&app, |u| {
                    if u.windows.is_empty() {
                        u.status = "error".into();
                    } else {
                        u.status = "stale".into();
                    }
                    u.note = msg;
                }),
            }
            // The 5-hour window moves at Claude-session speed: once a minute
            sleep_interruptible(POLL_SECS);
        }
    });
}

/// `codenotch.exe kimi` — one live round trip, printed (token and reading are not persisted).
pub fn check_live() -> String {
    let Some((host, port)) = live_instance() else {
        return "Kimi: no live local server (start the Kimi Code desktop app or `kimi web`)".into();
    };
    let Some(token) = read_token() else {
        return "Kimi: server found, but ~/.kimi-code/server.token is missing".into();
    };
    match fetch_once(&host, port, &token) {
        Ok(r) => {
            let mut o = format!("Kimi: ok ({}:{})\n", host, port);
            for w in &r.windows {
                o += &format!(
                    "  {}: {:.0}% used, resets {:?}\n",
                    w.label,
                    w.used * 100.0,
                    w.resets_at.map(|ms| ms / 1000)
                );
            }
            if !r.note.is_empty() {
                o += &format!("  {}\n", r.note);
            }
            o
        }
        Err(FetchErr::NeedsAuth) => "Kimi: the server rejected the session — sign in again".into(),
        Err(FetchErr::RateLimited(_)) => "Kimi: rate limited by the local server — try again in a minute.".into(),
        Err(FetchErr::Other(m)) => format!("Kimi: failed\n  {m}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live envelope observed from a real server (values verbatim).
    const PINNED: &str = r#"{"kind":"ok","quota":{"usages":{
        "limit5h":{"usedRatio":0.2738,"resetAt":"2026-11-03T18:00:21Z"},
        "monthTotal":{"usedRatio":0.0154,"resetAt":"2026-12-04T00:00:00Z"},
        "monthCode":{"usedRatio":0.0149,"resetAt":"2026-12-04T00:00:00Z"}},
        "extraUsage":null}}"#;

    fn value() -> serde_json::Value {
        serde_json::from_str(PINNED).unwrap()
    }

    #[test]
    fn the_three_quota_windows_map_to_cells() {
        let r = reading_from(&value()).unwrap();
        assert_eq!(r.windows.len(), 3);
        assert_eq!(r.windows[0].id, "limit5h");
        assert!((r.windows[0].used - 0.2738).abs() < 1e-9);
        assert_eq!(r.windows[0].label, "5-hour window");
        assert!(r.windows[0].resets_at.is_some());
        assert_eq!(r.windows[1].id, "monthCode");
        assert_eq!(r.windows[2].id, "monthTotal");
        assert!(r.note.is_empty(), "extraUsage null is not a note");
    }

    #[test]
    fn an_error_kind_is_an_error_not_a_reading() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"kind":"error","message":"not signed in"}"#).unwrap();
        assert!(reading_from(&v).unwrap_err().contains("not signed in"));
    }

    #[test]
    fn missing_usages_is_reported_not_invented() {
        let v: serde_json::Value = serde_json::from_str(r#"{"kind":"ok","quota":{"usages":{}}}"#).unwrap();
        let r = reading_from(&v).unwrap();
        assert!(r.windows.is_empty());
        assert!(r.note.contains("no quota windows"));
    }

    #[test]
    fn ratios_out_of_range_are_clamped() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"kind":"ok","quota":{"usages":{"limit5h":{"usedRatio":1.4,"resetAt":"2026-11-03T18:00:21Z"}}}}"#,
        )
        .unwrap();
        let r = reading_from(&v).unwrap();
        assert_eq!(r.windows[0].used, 1.0);
    }

    #[test]
    fn an_extra_usage_balance_becomes_a_note() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"kind":"ok","quota":{"usages":{"limit5h":{"usedRatio":0.1,"resetAt":"2026-11-03T18:00:21Z"}},"extraUsage":{"balance":25.0}}}"#,
        )
        .unwrap();
        let r = reading_from(&v).unwrap();
        assert!(r.note.contains("¥25.00"));
    }
}
