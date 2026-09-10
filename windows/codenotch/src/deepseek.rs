//! DeepSeek provider (official).
//!
//! Endpoint: GET https://api.deepseek.com/user/balance — Bearer <api key>, 15 s timeout.
//!
//! DeepSeek publishes no Claude-style rate-limit windows: the API is prepaid, so the only number
//! the account itself will tell you is what is left on it. The cell therefore shows the balance,
//! and when the user declares a budget the ring becomes a "budget used" percentage — that
//! denominator is ours, not the vendor's, so the window is flagged derived (the card adds "~").
//!
//! Reply shape, pinned by the tests at the bottom (they are what fails first if the endpoint moves):
//!   { "is_available": true,
//!     "balance_infos": [ { "currency": "CNY", "total_balance": "110.00",
//!                          "granted_balance": "10.00", "topped_up_balance": "100.00" } ] }
//!
//! The key is read, in order, from:
//!   1. %APPDATA%\codenotch\deepseek.json   { "api_key": "sk-…", "budget": 20.0 }
//!   2. the DEEPSEEK_API_KEY environment variable
//!   3. config.json                         { "deepseek_key": "sk-…", "deepseek_budget": 20.0 }
//! The app never writes a key anywhere, and the key is never logged or put in an error message.
//!
//! Failure discipline matches the Claude adapter: 401/403 → needsAuth, 429 → exponential backoff
//! with the deadline persisted, anything else → keep the last reading marked stale rather than
//! invent a number.

use crate::AppState;
use crate::usage::{LimitWindow, UsageSnapshot};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

/// Default API host; override with deepseek.json's "base_url" or DEEPSEEK_API_BASE (a relay,
/// a proxy, or a mock while developing). The path is always ours.
const DEFAULT_BASE: &str = "https://api.deepseek.com";
const POLL_SECS: u64 = 300;
const BACKOFF_BASE_SECS: u64 = 60;
const BACKOFF_CAP_SECS: u64 = 900;

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

/// User-owned settings file (the key lives here rather than in config.json when it can)
fn settings_path() -> std::path::PathBuf {
    crate::config::config_path().with_file_name("deepseek.json")
}

/// The reading the app keeps between launches (its own file, so a settings edit cannot clobber it)
fn store_path() -> std::path::PathBuf {
    crate::config::config_path().with_file_name("usage-deepseek.json")
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    #[serde(default)]
    pub api_key: String,
    /// Declared prepaid budget, in the balance's own currency; <= 0 means "no percentage"
    #[serde(default)]
    pub budget: f64,
    /// API host to talk to; empty = https://api.deepseek.com (a relay or a proxy goes here)
    #[serde(default)]
    pub base_url: String,
}

/// Where the balance is read from. The key only ever goes to this host, so a wrong value here hands
/// it to a stranger — hence the explicit, documented name rather than a magic test-only override.
fn endpoint(s: &Settings) -> String {
    let base = if !s.base_url.trim().is_empty() {
        s.base_url.trim().to_string()
    } else {
        std::env::var("DEEPSEEK_API_BASE").unwrap_or_default().trim().to_string()
    };
    let base = base.trim_end_matches('/').to_string();
    if base.is_empty() {
        format!("{DEFAULT_BASE}/user/balance")
    } else {
        format!("{base}/user/balance")
    }
}

/// The settings file first, then the environment, then config.json
pub fn settings() -> Settings {
    let mut s = std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|t| serde_json::from_str::<Settings>(&t).ok())
        .unwrap_or_default();
    let cfg = crate::config::load();
    if s.api_key.trim().is_empty() {
        s.api_key = cfg.deepseek_key.clone();
    }
    if s.budget <= 0.0 {
        s.budget = cfg.deepseek_budget;
    }
    if s.api_key.trim().is_empty() {
        if let Ok(k) = std::env::var("DEEPSEEK_API_KEY") {
            s.api_key = k;
        }
    }
    s.api_key = s.api_key.trim().to_string();
    s
}

/// True when a key exists — the page only draws a cell then
pub fn configured() -> bool {
    !settings().api_key.is_empty()
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

/// For doctor / the tray notice: says whether a key is present, never what it is
pub fn probe() -> String {
    let s = settings();
    if s.api_key.is_empty() {
        format!(
            "DeepSeek: no key. Put {{\"api_key\":\"sk-…\"}} in {} (or set DEEPSEEK_API_KEY)",
            settings_path().display()
        )
    } else {
        format!(
            "DeepSeek: key present ({} chars), budget {}, endpoint {}",
            s.api_key.len(),
            if s.budget > 0.0 { format!("{}", s.budget) } else { "not declared (the cell shows the balance)".into() },
            endpoint(&s)
        )
    }
}

/// `codenotch.exe deepseek [key]` — one live round trip, printed. The optional argument lets the
/// check run before the key is installed in deepseek.json; the key itself is never echoed back, and
/// neither the reading nor the key is written to any file by this path.
pub fn check_live(key_arg: &str) -> String {
    let mut s = settings();
    if !key_arg.trim().is_empty() {
        s.api_key = key_arg.trim().to_string();
    }
    let ep = endpoint(&s);
    if s.api_key.is_empty() {
        return format!(
            "DeepSeek: no key\n  endpoint: {ep}\n  put {{\"api_key\":\"sk-…\"}} in {}",
            settings_path().display()
        );
    }
    match fetch_once(&s.api_key) {
        Ok(r) => {
            let mut o = format!("DeepSeek: ok\n  endpoint: {ep}\n");
            for w in &r.windows {
                o += &match &w.text {
                    Some(t) => format!("  {}: {t}\n", w.label),
                    None => format!("  {}: ~{:.0}% used\n", w.label, w.used * 100.0),
                };
            }
            if !r.note.is_empty() {
                o += &format!("  {}\n", r.note);
            }
            o
        }
        // Deliberately says only what was observed: DeepSeek answers 401 for an unknown path too,
        // so a rejection proves the host is reachable and the key is not accepted — nothing about
        // the path. The path comes from the published API (api-docs.deepseek.com/api/get-user-balance).
        Err(FetchErr::NeedsAuth) => format!(
            "DeepSeek: {ep} answered 401/403 — the host is reachable, the key was not accepted.\n  (A balance is only returned for a valid key; the path follows the published API.)"
        ),
        Err(FetchErr::RateLimited(_)) => format!("DeepSeek: rate limited by {ep} — try again in a minute."),
        Err(FetchErr::Other(m)) => format!("DeepSeek: failed against {ep}\n  {m}"),
    }
}

#[derive(Debug, Clone, Default)]
pub struct Reading {
    pub windows: Vec<LimitWindow>,
    pub note: String,
}

/// "110.00" (a JSON string) or 110.0 (a number) — the endpoint has used both shapes for money
fn amount(b: &serde_json::Value, key: &str) -> f64 {
    match b.get(key) {
        Some(serde_json::Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        _ => 0.0,
    }
}

pub fn symbol(currency: &str) -> &'static str {
    match currency {
        "CNY" => "¥",
        "USD" => "$",
        _ => "",
    }
}

/// Money in the account's own currency, two decimals, symbol in front when we know it
pub fn money(currency: &str, v: f64) -> String {
    let sym = symbol(currency);
    let n = format!("{v:.2}");
    if sym.is_empty() {
        if currency.is_empty() { n } else { format!("{n} {currency}") }
    } else {
        format!("{sym}{n}")
    }
}

/// The wire reply → the cell. Pure, so the shape is pinned by a test instead of by a live account.
pub fn reading_from(v: &serde_json::Value, budget: f64) -> Result<Reading, String> {
    let infos = v
        .get("balance_infos")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    if infos.is_empty() {
        // Reachable account, nothing to report: say so rather than draw a zero
        return Ok(Reading {
            windows: Vec::new(),
            note: "DeepSeek returned no balance information for this key".into(),
        });
    }
    let b = &infos[0];
    let currency = b.get("currency").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let total = amount(b, "total_balance");
    let granted = amount(b, "granted_balance");
    let topped = amount(b, "topped_up_balance");
    let available = v.get("is_available").and_then(|x| x.as_bool()).unwrap_or(true);

    let mut windows = vec![LimitWindow {
        id: "balance".into(),
        label: "Balance".into(),
        text: Some(money(&currency, total)),
        ..Default::default()
    }];
    let mut note = String::new();
    if budget > 0.0 {
        // Derived on purpose: DeepSeek has no monthly allowance, the budget is the user's own line
        let used = ((budget - total) / budget).clamp(0.0, 1.0);
        windows.push(LimitWindow {
            id: "budget".into(),
            label: "Declared budget".into(),
            used,
            derived: true,
            ..Default::default()
        });
        note = format!(
            "{} left of the {} budget you declared",
            money(&currency, total.max(0.0)),
            money(&currency, budget)
        );
    } else if granted > 0.0 {
        note = format!("{} granted + {} topped up", money(&currency, granted), money(&currency, topped));
    }
    if !available {
        note = format!(
            "Account unavailable — top up to keep the API answering ({}){}",
            money(&currency, total),
            if note.is_empty() { String::new() } else { format!(" · {note}") }
        );
    }
    Ok(Reading { windows, note })
}

enum FetchErr {
    NeedsAuth,
    RateLimited(u64),
    Other(String),
}

fn fetch_once(key: &str) -> Result<Reading, FetchErr> {
    let s = settings();
    let resp = ureq::get(&endpoint(&s))
        .set("Authorization", &format!("Bearer {key}"))
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(15))
        .call();
    match resp {
        Ok(r) => {
            let v: serde_json::Value = r
                .into_json()
                .map_err(|e| FetchErr::Other(format!("parse: {e}")))?;
            reading_from(&v, s.budget).map_err(FetchErr::Other)
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
        let mut u = st.deepseek.lock().unwrap();
        mutate(&mut u);
        u.clone()
    };
    persist(&snap);
    let _ = app.emit("deepseek", &snap);
}

pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        // Absent key = no cell at all; announce that state once so the page never waits on a hidden provider
        if !configured() {
            set_and_broadcast(&app, |u| {
                u.status = "absent".into();
                u.note = "No DeepSeek API key configured".into();
            });
            // A key can appear while the app runs (env change, edited file, a fresh deepseek.json):
            // re-check every 30 s, and at once when the tray's Refresh is used.
            loop {
                sleep_interruptible(30);
                if configured() {
                    break;
                }
            }
        }
        {
            let st = app.state::<AppState>();
            let snap = st.deepseek.lock().unwrap().clone();
            if !snap.windows.is_empty() {
                let _ = app.emit("deepseek", &snap);
            }
        }
        let mut consecutive_429: u32 = 0;
        loop {
            let bu = {
                let st = app.state::<AppState>();
                let u = st.deepseek.lock().unwrap();
                u.backoff_until
            };
            let now = now_ms();
            if bu > now {
                sleep_interruptible(((bu - now) / 1000).clamp(1, 30));
                continue;
            }
            let s = settings();
            if s.api_key.is_empty() {
                set_and_broadcast(&app, |u| {
                    u.status = "absent".into();
                    u.note = "No DeepSeek API key configured".into();
                    u.windows.clear();
                });
                sleep_interruptible(POLL_SECS);
                continue;
            }
            match fetch_once(&s.api_key) {
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
                    u.note = "The key was rejected — check it at platform.deepseek.com/api_keys".into();
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
            // A balance moves far slower than a rate-limit window: one poll per five minutes is plenty
            let active = {
                let st = app.state::<AppState>();
                let store = st.store.lock().unwrap();
                let s = store.snapshot("en", "en", false);
                !s.sessions.is_empty()
            };
            sleep_interruptible(if active { 60 } else { POLL_SECS });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reply shape published at api-docs.deepseek.com/api/get-user-balance, with the example
    /// values from the same page verbatim — this is the test that fails first if the endpoint moves.
    const PINNED: &str = r#"{"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"110.00","granted_balance":"10.00","topped_up_balance":"100.00"}]}"#;

    fn value() -> serde_json::Value {
        serde_json::from_str(PINNED).unwrap()
    }

    #[test]
    fn the_vendor_balance_is_shown_untouched_when_no_budget_is_declared() {
        let r = reading_from(&value(), 0.0).unwrap();
        assert_eq!(r.windows.len(), 1);
        assert_eq!(r.windows[0].id, "balance");
        assert_eq!(r.windows[0].text.as_deref(), Some("¥110.00"));
        assert!(r.windows[0].only_a_readout());
        // 10 granted + 100 topped up is where the 110 came from, and the card can say so
        assert!(r.note.contains("¥10.00 granted"));
    }

    #[test]
    fn a_declared_budget_turns_the_ring_into_our_own_percentage() {
        let r = reading_from(&value(), 220.0).unwrap();
        assert_eq!(r.windows.len(), 2);
        let b = r.windows.iter().find(|w| w.id == "budget").unwrap();
        assert!((b.used - 0.5).abs() < 1e-9, "110 of a 220 budget is half spent");
        assert!(b.derived, "the denominator is ours, not DeepSeek's");
        assert_eq!(b.text, None);
        assert!(r.note.contains("¥110.00 left"));
    }

    #[test]
    fn a_balance_over_the_budget_never_draws_a_negative_ring() {
        let r = reading_from(&value(), 50.0).unwrap();
        let b = r.windows.iter().find(|w| w.id == "budget").unwrap();
        assert_eq!(b.used, 0.0);
    }

    #[test]
    fn a_negative_balance_clamps_to_a_full_ring() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"is_available":false,"balance_infos":[{"currency":"USD","total_balance":"-3.50","granted_balance":"0.00","topped_up_balance":"0.00"}]}"#,
        )
        .unwrap();
        let r = reading_from(&v, 100.0).unwrap();
        assert_eq!(r.windows[0].text.as_deref(), Some("$-3.50"));
        let b = r.windows.iter().find(|w| w.id == "budget").unwrap();
        assert_eq!(b.used, 1.0);
        assert!(r.note.starts_with("Account unavailable"));
    }

    #[test]
    fn money_with_an_unknown_currency_keeps_the_code_rather_than_guessing_a_symbol() {
        assert_eq!(money("EUR", 12.5), "12.50 EUR");
        assert_eq!(money("", 12.5), "12.50");
        assert_eq!(money("USD", 12.5), "$12.50");
    }

    #[test]
    fn numbers_sent_as_json_numbers_parse_too() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"is_available":true,"balance_infos":[{"currency":"USD","total_balance":7.25}]}"#).unwrap();
        let r = reading_from(&v, 0.0).unwrap();
        assert_eq!(r.windows[0].text.as_deref(), Some("$7.25"));
    }

    #[test]
    fn the_endpoint_is_the_api_host_plus_our_own_path() {
        let mut s = Settings::default();
        assert_eq!(endpoint(&s), "https://api.deepseek.com/user/balance");
        s.base_url = "https://relay.example.com/".into();
        assert_eq!(endpoint(&s), "https://relay.example.com/user/balance");
    }

    #[test]
    fn an_empty_balance_list_is_reported_not_invented() {
        let v: serde_json::Value = serde_json::from_str(r#"{"is_available":true,"balance_infos":[]}"#).unwrap();
        let r = reading_from(&v, 0.0).unwrap();
        assert!(r.windows.is_empty());
        assert!(r.note.contains("no balance information"));
    }
}
