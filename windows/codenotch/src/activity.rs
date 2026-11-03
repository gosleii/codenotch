//! "Is it working?" for the providers (the usage rings come from the adapters; this is the
//! working-state overlay).
//!
//! None of the three has a state field of its own, so each is labelled with whatever it
//! can honestly provide (the same trade-off upstream made):
//!   - Cursor: the `composerHeaders` rows (JSON) in the editor's `state.vscdb` — `unfinishedRunAt`
//!     is set for the duration of a run and cleared when it ends; `hasBlockingPendingActions` /
//!     `hasPendingPlan` = waiting on you. This is **real state**. The database is in WAL mode, so
//!     it must be opened as a plain read-only connection (immutable ignores the WAL and shows the
//!     world as of the last checkpoint).
//!   - Codex: the desktop app keeps turn state in `thread_turns` inside
//!     `~/.codex/thread_history_1.sqlite` (status = inProgress with an empty completed_at = running)
//!     — real state. The CLI / VS Code extension fall back to classifying the last entry of the
//!     rollout, with a silence threshold that depends on the entry type.
//!   - Antigravity: transcript.jsonl is appended during a run (each step is written only once it
//!     completes, so status is always DONE and useless); written within the last 45 s = working
//!     (the model can think for a long time between steps, hence the wide window).
//! Polled every 2 s (upstream cadence), broadcast only on change. Cost discipline: database
//! connections stay open, nothing is re-queried unless the file's mtime changed, the rollout tail
//! is re-read only when its mtime changed, and the thread runs at lowered priority.

use crate::AppState;
use serde::Serialize;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const INTERVAL: Duration = Duration::from_secs(2);
const ANTIGRAVITY_STALE_MS: u64 = 45_000;

#[derive(Clone, Serialize, Debug, PartialEq)]
pub struct Activity {
    /// Provider id: codex / cursor / gemini / deepseek
    pub provider: String,
    /// busy | waiting
    pub state: String,
    pub name: String,
    pub detail: String,
    /// ms epoch
    pub since: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn mtime_ms(p: &std::path::Path) -> Option<u64> {
    std::fs::metadata(p)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}


/// Persistent connection + change gating: the query runs again only when the database file (or its
/// -wal) changed mtime; otherwise the last result is reused. Cursor's state.vscdb is over 2 GB, and
/// reopening it every 2 s for a table scan slowed the whole machine (typing lagged).
struct DbCache {
    path: std::path::PathBuf,
    conn: Option<rusqlite::Connection>,
    sig: (u64, u64),
    last: Vec<Activity>,
    checked_once: bool,
}

impl DbCache {
    fn new(path: std::path::PathBuf) -> Self {
        Self { path, conn: None, sig: (0, 0), last: Vec::new(), checked_once: false }
    }
    fn signature(&self) -> (u64, u64) {
        let wal = {
            let mut o = self.path.as_os_str().to_owned();
            o.push("-wal");
            std::path::PathBuf::from(o)
        };
        (mtime_ms(&self.path).unwrap_or(0), mtime_ms(&wal).unwrap_or(0))
    }
    /// Calls f only when something changed (or on the first run); f returning None means the query failed → drop the connection and reopen next time
    fn refresh<F: FnOnce(&rusqlite::Connection) -> Option<Vec<Activity>>>(&mut self, f: F) -> Vec<Activity> {
        let sig = self.signature();
        if self.checked_once && sig == self.sig {
            return self.last.clone();
        }
        self.sig = sig;
        self.checked_once = true;
        if self.conn.is_none() {
            self.conn = open_ro(&self.path);
        }
        let Some(conn) = self.conn.as_ref() else {
            self.last.clear();
            return Vec::new();
        };
        match f(conn) {
            Some(v) => self.last = v,
            None => {
                self.conn = None;
                self.last.clear();
            }
        }
        self.last.clone()
    }
}

/// Everything the probe thread keeps between ticks
struct Ctx {
    cursor: DbCache,
    codex_turns: DbCache,
    codex_names: Option<rusqlite::Connection>,
    rollout_path: Option<std::path::PathBuf>,
    rollout_checked_at: u64,
    rollout_sig: u64,
    rollout_step: Option<(CodexStep, u64)>,
}

impl Ctx {
    fn new() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        Self {
            cursor: DbCache::new(crate::cursor::store_url().unwrap_or_default()),
            codex_turns: DbCache::new(home.join(".codex").join("thread_history_1.sqlite")),
            codex_names: None,
            rollout_path: None,
            rollout_checked_at: 0,
            rollout_sig: 0,
            rollout_step: None,
        }
    }
}

// ---------------- Cursor ----------------

fn open_ro(path: &std::path::Path) -> Option<rusqlite::Connection> {
    use rusqlite::OpenFlags;
    if !path.is_file() {
        return None;
    }
    rusqlite::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()
}

fn cursor_activity(ctx: &mut Ctx) -> Vec<Activity> {
    ctx.cursor.refresh(|conn| {
        let mut stmt = conn.prepare("SELECT value FROM composerHeaders WHERE isArchived = 0 ORDER BY recency DESC LIMIT 40").ok()?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).ok()?;
        let mut out = Vec::new();
        for json in rows.flatten() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) else { continue };
            if v.get("composerId").and_then(|x| x.as_str()).is_none() {
                continue;
            }
            let blocked = v.get("hasBlockingPendingActions").and_then(|x| x.as_bool()) == Some(true)
                || v.get("hasPendingPlan").and_then(|x| x.as_bool()) == Some(true);
            let running = v.get("unfinishedRunAt").and_then(|x| x.as_f64());
            let state = if blocked {
                "waiting"
            } else if running.is_some() {
                "busy"
            } else {
                continue; // forty idle past conversations are not forty things happening now
            };
            let since = running
                .or_else(|| v.get("lastUpdatedAt").and_then(|x| x.as_f64()))
                .or_else(|| v.get("createdAt").and_then(|x| x.as_f64()))
                .map(|ms| ms as u64)
                .unwrap_or_else(now_ms);
            out.push(Activity {
                provider: "cursor".into(),
                state: state.into(),
                name: v.get("name").and_then(|x| x.as_str()).unwrap_or("Untitled chat").to_string(),
                detail: if blocked {
                    "needs your input".into()
                } else {
                    v.get("subtitle").and_then(|x| x.as_str()).unwrap_or("Working").to_string()
                },
                since,
            });
        }
        out.sort_by(|a, b| b.since.cmp(&a.since));
        Some(out)
    })
}

// ---------------- Codex ----------------

/// The last meaningful entry at the tail of a rollout says which step Codex is on.
/// Lines look like {"timestamp","type":"response_item"|"turn_context"|"event_msg"|…,"payload":{…}};
/// task_started/task_complete events are not always written, so the decision rests on the entry
/// type plus how long the file has been silent:
///   function call (a tool is running, or waiting for your approval) → busy, for up to 10 minutes;
///   tool output / user message / turn context / reasoning → the model is deciding the next step,
///   busy while silent for < 120 s (long thinking has to be tolerated);
///   assistant message → could be the final answer or narration along the way, busy while silent for < 4 s;
///   turn_aborted → idle. Bookkeeping lines such as token_count are skipped.
#[derive(Clone, Copy, PartialEq, Debug)]
enum CodexStep {
    Tool,
    Thinking,
    AsstMsg,
    Aborted,
}

fn codex_last_step(text: &str) -> Option<(CodexStep, u64)> {
    for line in text.lines().rev().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let ts = v
            .get("timestamp")
            .and_then(|x| x.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp_millis().max(0) as u64)
            .unwrap_or(0);
        let kind = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let p = v.get("payload").cloned().unwrap_or(serde_json::Value::Null);
        let pt = p.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let step = match kind {
            "turn_context" => Some(CodexStep::Thinking),
            "response_item" => match pt {
                "function_call" | "local_shell_call" | "custom_tool_call" | "web_search_call" => Some(CodexStep::Tool),
                "function_call_output" | "custom_tool_call_output" | "reasoning" => Some(CodexStep::Thinking),
                "message" => match p.get("role").and_then(|x| x.as_str()).unwrap_or("") {
                    "assistant" if p.get("channel").and_then(|x| x.as_str()) == Some("final") => Some(CodexStep::Aborted),
                    "assistant" => Some(CodexStep::AsstMsg),
                    "user" => Some(CodexStep::Thinking),
                    _ => None, // system/developer messages say nothing about state
                },
                _ => None,
            },
            "event_msg" => match pt {
                "turn_aborted" | "task_complete" | "task_completed" | "turn_complete" | "turn_failed" => Some(CodexStep::Aborted),
                "task_started" | "item_started" | "exec_command_begin" => Some(CodexStep::Thinking),
                "user_message" => Some(CodexStep::Thinking),
                "agent_message" if p.get("phase").and_then(|x| x.as_str()) == Some("final_answer") => Some(CodexStep::Aborted),
                "agent_message" => Some(CodexStep::AsstMsg),
                "agent_reasoning" | "agent_reasoning_raw_content" => Some(CodexStep::Thinking),
                _ => None, // token_count and other bookkeeping lines
            },
            _ => None,
        };
        if let Some(st) = step {
            return Some((st, ts));
        }
    }
    None
}

/// The desktop app's real state: table `thread_turns` in `~/.codex/thread_history_1.sqlite`
/// (status = inProgress / completed…, started_at in seconds, empty completed_at = still running).
/// The app maintains this turn table itself, which is far more reliable than a file mtime. Guard
/// against "inProgress forever after a crash": no new item for the thread in the last 10 minutes
/// (`thread_items.created_at_ms`) while the turn started more than 2 minutes ago → treated as stale.
fn codex_turns_in_progress(ctx: &mut Ctx) -> Vec<Activity> {
    let now = now_ms();
    if ctx.codex_names.is_none() {
        ctx.codex_names = dirs::home_dir().and_then(|h| open_ro(&h.join(".codex").join("state_5.sqlite")));
    }
    let names = ctx.codex_names.as_ref();
    // Unlike Cursor's cached UI flags, Codex freshness expires with time even if the DB is unchanged.
    ctx.codex_turns.checked_once = false;
    ctx.codex_turns.refresh(|conn| desktop_codex_activity(conn, names, now))
}

fn desktop_codex_activity(conn: &rusqlite::Connection, names: Option<&rusqlite::Connection>, now: u64) -> Option<Vec<Activity>> {
        let mut stmt = conn
            .prepare("SELECT t.thread_id, t.started_at, t.turn_id FROM thread_turns t
                WHERE t.status = 'inProgress' AND t.completed_at IS NULL AND t.started_at > 0
                  AND NOT EXISTS (SELECT 1 FROM thread_turns newer WHERE newer.thread_id = t.thread_id
                    AND (newer.started_at > t.started_at OR (newer.started_at = t.started_at
                        AND COALESCE(newer.rollout_ordinal,0) > COALESCE(t.rollout_ordinal,0))))
                ORDER BY t.started_at DESC LIMIT 8")
            .ok()?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, rusqlite::types::Value>(1)?,r.get::<_, String>(2)?))).ok()?;
        let mut out = Vec::new();
        for (thread_id, started, turn_id) in rows.flatten() {
            let started_ms = match started {
                rusqlite::types::Value::Integer(i) => (i as u64) * if i > 10_000_000_000 { 1 } else { 1000 },
                rusqlite::types::Value::Real(f) => (f * if f > 10_000_000_000.0 { 1.0 } else { 1000.0 }) as u64,
                _ => 0,
            };
            // The thread's latest item: freshness, and whether it is waiting for approval
            let (last_ms, last_type): (Option<i64>, Option<String>) = conn
                .query_row(
                    "SELECT created_at_ms, item_type FROM thread_items WHERE thread_id = ?1 AND turn_id = ?2 ORDER BY created_at_ms DESC LIMIT 1",
                    [&thread_id, &turn_id],
                    |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<String>>(1)?)),
                )
                .unwrap_or((None, None));
            let last = last_ms.map(|v| v as u64).unwrap_or(started_ms);
            let fresh = now.saturating_sub(last) <= 10 * 60_000 || now.saturating_sub(started_ms) <= 2 * 60_000;
            if !fresh {
                continue;
            }
            let mut name = String::new();
            if let Some(c) = names {
                if let Ok((title, first, nick)) = c.query_row(
                    "SELECT COALESCE(title,''), COALESCE(first_user_message,''), COALESCE(agent_nickname,'') FROM threads WHERE id = ?1",
                    [&thread_id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
                ) {
                    name = if !title.trim().is_empty() {
                        title
                    } else if !first.trim().is_empty() {
                        first.chars().take(40).collect()
                    } else if !nick.trim().is_empty() {
                        format!("Agent {nick}")
                    } else {
                        String::new()
                    };
                }
            }
            if name.is_empty() {
                name = "Codex".into();
            }
            let lt = last_type.unwrap_or_default().to_lowercase();
            let waiting = lt.contains("approval") || lt.contains("permission") || lt.contains("request_user");
            out.push(Activity {
                provider: "codex".into(),
                state: if waiting { "waiting" } else { "busy" }.into(),
                name,
                detail: if waiting { "needs your input".into() } else { "Working".into() },
                since: started_ms,
            });
        }
        Some(out)
}

fn rollout_busy(step: Option<(CodexStep, u64)>, now: u64) -> bool {
    let Some((step, at)) = step.filter(|(_, at)| *at > 0) else { return false };
    let quiet = now.saturating_sub(at);
    match step {
        CodexStep::Tool => quiet <= 10 * 60_000,
        CodexStep::Thinking => quiet <= 120_000,
        CodexStep::AsstMsg => quiet <= 4_000,
        CodexStep::Aborted => false,
    }
}

fn rollout_is_completed(path: &std::path::Path, conn: Option<&rusqlite::Connection>, step: Option<(CodexStep,u64)>) -> bool {
    let Some(conn) = conn else { return false };
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { return false };
    let Some(id) = stem.len().checked_sub(36).and_then(|at| stem.get(at..)) else { return false };
    conn.query_row("SELECT status, completed_at FROM thread_turns WHERE thread_id = ?1 ORDER BY started_at DESC, rollout_ordinal DESC LIMIT 1",
        [id], |r| Ok((r.get::<_, String>(0)?,r.get::<_, Option<i64>>(1)?)))
        .map(|(status,completed)| {
            let completed=completed.unwrap_or(0).max(0) as u64;
            let completed=if completed > 10_000_000_000 { completed } else { completed*1000 };
            status != "inProgress" && step.map(|(_,at)| at <= completed).unwrap_or(false)
        }).unwrap_or(false)
}

fn codex_activity(ctx: &mut Ctx) -> Vec<Activity> {
    // 1. The desktop app's real state
    let turns = codex_turns_in_progress(ctx);
    if !turns.is_empty() {
        return turns;
    }
    // 2. CLI / extension: locate the rollout every 30 s; skip the 256 KB tail read when its mtime has not changed
    let now = now_ms();
    if now.saturating_sub(ctx.rollout_checked_at) > 30_000 || ctx.rollout_path.is_none() {
        ctx.rollout_checked_at = now;
        let path = crate::codex::newest_rollout();
        if path != ctx.rollout_path {
            ctx.rollout_sig = 0;
            ctx.rollout_step = None;
        }
        ctx.rollout_path = path;
    }
    let Some(p) = ctx.rollout_path.clone() else { return vec![] };
    let mtime = mtime_ms(&p).unwrap_or(0);
    if mtime != ctx.rollout_sig {
        ctx.rollout_sig = mtime;
        ctx.rollout_step = crate::codex::tail_text(&p).and_then(|text| codex_last_step(&text));
    }
    // Completed desktop state wins over older transcript entries, but permits a later CLI resume.
    if rollout_is_completed(&p, ctx.codex_turns.conn.as_ref(), ctx.rollout_step) { return vec![]; }
    if rollout_busy(ctx.rollout_step, now) {
        vec![Activity { provider: "codex".into(), state: "busy".into(), name: "Codex".into(),
            detail: "Working".into(), since: ctx.rollout_step.unwrap().1 }]
    } else {
        vec![]
    }
}

// ---------------- Antigravity ----------------

fn antigravity_activity() -> Vec<Activity> {
    let Some(root) = dirs::home_dir().map(|h| h.join(".gemini").join("antigravity").join("brain")) else { return vec![] };
    let Ok(rd) = std::fs::read_dir(&root) else { return vec![] };
    let mut newest: Option<(String, u64)> = None;
    for e in rd.flatten() {
        let t = e.path().join(".system_generated").join("logs").join("transcript.jsonl");
        let Some(m) = mtime_ms(&t) else { continue };
        if newest.as_ref().map(|(_, n)| m > *n).unwrap_or(true) {
            newest = Some((e.file_name().to_string_lossy().to_string(), m));
        }
    }
    let Some((_, at)) = newest else { return vec![] };
    if now_ms().saturating_sub(at) > ANTIGRAVITY_STALE_MS {
        return vec![];
    }
    vec![Activity { provider: "gemini".into(), state: "busy".into(), name: "Antigravity".into(), detail: "Working".into(), since: at }]
}

// ---------------- DeepSeek ----------------

/// The DeepSeek cell has no local CLI behind it, but the DSH harness on this machine is a DeepSeek
/// client, and it leaves a session transcript behind while it works. That is the same class of
/// evidence as Antigravity's transcripts: an inference from recent writes, never claimed as a
/// reading from DeepSeek itself. The row in the card names the writer, so nobody has to guess.
///
/// Staleness is generous on purpose: a turn spends most of its wall time streaming one response,
/// and a long tool call can leave the transcript untouched for a minute without the agent being
/// idle. 90 s says "working" through that and still drops the arc soon after a turn ends.
const DSH_STALE_MS: u64 = 90_000;

/// A transcript file worth timing: "slug · when" is all the cell needs to name the session, and
/// nothing here is ever uploaded anywhere.
fn transcript_candidate(p: &std::path::Path) -> Option<(String, u64)> {
    let name = p.file_name()?.to_string_lossy().to_string();
    if !(name.ends_with(".jsonl") || name.ends_with(".jsonl.zstd")) {
        return None;
    }
    let at = mtime_ms(p)?;
    // sessions/<slug>/<session id>/session.jsonl[.zstd] — the slug is the readable one
    let label = p
        .parent()
        .and_then(|d| d.parent())
        .and_then(|d| d.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| name.clone());
    Some((label, at))
}

/// Walks the transcript tree and returns the most recently written file's label and mtime.
fn newest_transcript(dir: &std::path::Path, depth: usize) -> Option<(String, u64)> {
    if depth > 4 {
        return None;
    }
    let rd = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(String, u64)> = None;
    for e in rd.flatten() {
        let p = e.path();
        let found = if p.is_dir() { newest_transcript(&p, depth + 1) } else { transcript_candidate(&p) };
        if let Some((label, at)) = found {
            if best.as_ref().map(|(_, n)| at > *n).unwrap_or(true) {
                best = Some((label, at));
            }
        }
    }
    best
}

fn dsh_activity() -> Vec<Activity> {
    let Some(root) = dirs::home_dir().map(|h| h.join(".dsh").join("sessions")) else { return vec![] };
    if !root.is_dir() {
        return vec![];
    }
    let Some((label, at)) = newest_transcript(&root, 0) else { return vec![] };
    if now_ms().saturating_sub(at) > DSH_STALE_MS {
        return vec![];
    }
    let label = label.trim_matches('-').replace('-', "/");
    let label = if label.is_empty() { "session".to_string() } else { label };
    vec![Activity {
        provider: "deepseek".into(),
        state: "busy".into(),
        name: format!("DSH · {label}"),
        detail: "Working".into(),
        since: at,
    }]
}

// ---------------- Putting it together ----------------

#[derive(Clone, Copy, Default)]
pub struct Presence {
    cursor: bool,
    codex: bool,
    gemini: bool,
}

fn presence() -> Presence {
    // A provider switched off in the tray is not probed either: its cell is gone, so a probe would
    // only cost (Cursor's reads a multi-gigabyte SQLite database, Antigravity's the local bridge).
    let hidden = crate::config::load().hidden;
    let off = |id: &str| hidden.iter().any(|h| h == id);
    Presence {
        cursor: !off("cursor") && crate::cursor::present(),
        codex: !off("codex") && crate::codex::present(),
        gemini: !off("gemini") && crate::antigravity::present(),
    }
}

fn read_all(p: Presence, ctx: &mut Ctx) -> Vec<Activity> {
    let mut all = Vec::new();
    if p.cursor {
        all.extend(cursor_activity(ctx));
    }
    if p.codex {
        all.extend(codex_activity(ctx));
    }
    if p.gemini {
        all.extend(antigravity_activity());
    }
    // Not gated on presence: the harness is found by its transcripts alone, and an absent one is an
    // empty list rather than an error
    all.extend(dsh_activity());
    all
}

/// For doctor: the raw material behind the Codex working-state decision
pub fn probe() -> String {
    let now = now_ms();
    let Some(p) = crate::codex::newest_rollout() else { return "Codex activity: no rollout found".into() };
    let age = now.saturating_sub(mtime_ms(&p).unwrap_or(0)) / 1000;
    let step = crate::codex::tail_text(&p).and_then(|t| codex_last_step(&t));
    let tail: Vec<String> = crate::codex::tail_text(&p)
        .map(|t| {
            t.lines()
                .rev()
                .filter(|l| !l.trim().is_empty())
                .take(6)
                .map(|l| {
                    serde_json::from_str::<serde_json::Value>(l)
                        .map(|v| {
                            format!(
                                "{}/{}/{}",
                                v.get("type").and_then(|x| x.as_str()).unwrap_or("?"),
                                v.pointer("/payload/type").and_then(|x| x.as_str()).unwrap_or("-"),
                                v.pointer("/payload/role").and_then(|x| x.as_str()).unwrap_or("-")
                            )
                        })
                        .unwrap_or_else(|_| "(not a JSON line)".into())
                })
                .collect()
        })
        .unwrap_or_default();
    format!(
        "Codex activity: rollout={} modified {age}s ago | last step={:?} | last 6 lines (type/payload.type/role)=[{}]",
        p.display(),
        step.map(|(s, ts)| format!("{s:?} @{}s ago", now.saturating_sub(ts) / 1000)),
        tail.join(", ")
    )
}

#[cfg(windows)]
pub fn lower_thread_priority() {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}
#[cfg(not(windows))]
pub fn lower_thread_priority() {}

pub fn start(app: AppHandle) {
    std::thread::spawn(move || {
        lower_thread_priority(); // the probe always yields to foreground input
        let mut ctx = Ctx::new();
        let mut last: Vec<Activity> = Vec::new();
        let mut pres = presence();
        let mut tick: u32 = 0;
        loop {
            // Presence checks (finding the exe, reading credentials) once a minute are plenty; the 2 s tick does only stats and a query
            if tick % 30 == 0 {
                pres = presence();
            }
            tick = tick.wrapping_add(1);
            let found = read_all(pres, &mut ctx);
            if found != last {
                // Log the first 20 state changes (with the Codex raw material) so thresholds can be calibrated
                static LOGGED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if LOGGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 20 {
                    crate::applog(&format!("activity: {:?} | {}", found.iter().map(|a| format!("{}:{}", a.provider, a.state)).collect::<Vec<_>>(), probe()));
                }
                last = found.clone();
                {
                    let st = app.state::<AppState>();
                    *st.activity.lock().unwrap() = found.clone();
                }
                let _ = app.emit("activity", &found);
            }
            std::thread::sleep(INTERVAL);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn database() -> rusqlite::Connection {
        let c=rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE thread_turns(thread_id TEXT,turn_id TEXT,status TEXT,started_at INTEGER,completed_at INTEGER,rollout_ordinal INTEGER);
            CREATE TABLE thread_items(thread_id TEXT,turn_id TEXT,created_at_ms INTEGER,item_type TEXT);").unwrap();
        c
    }

    #[test]
    fn completed_new_turn_prevents_old_orphan_from_becoming_active() {
        let c=database();
        c.execute_batch("INSERT INTO thread_turns VALUES('chat','orphan','inProgress',100,NULL,1);
            INSERT INTO thread_turns VALUES('chat','new','completed',200,201,2);
            INSERT INTO thread_items VALUES('chat','new',201000,'agentMessage');").unwrap();
        assert!(desktop_codex_activity(&c,None,202000).unwrap().is_empty());
    }

    #[test]
    fn turns_in_the_same_second_use_rollout_order() {
        let c=database();
        c.execute_batch("INSERT INTO thread_turns VALUES('chat','orphan','inProgress',200,NULL,1);
            INSERT INTO thread_turns VALUES('chat','new','completed',200,201,2);").unwrap();
        assert!(desktop_codex_activity(&c,None,202000).unwrap().is_empty());
    }

    #[test]
    fn unrelated_turn_items_cannot_keep_an_orphan_fresh() {
        let c=database();
        c.execute_batch("INSERT INTO thread_turns VALUES('chat','orphan','inProgress',100,NULL,1);
            INSERT INTO thread_items VALUES('chat','orphan',100000,'agentMessage');
            INSERT INTO thread_items VALUES('chat','different',1000000,'agentMessage');").unwrap();
        assert!(desktop_codex_activity(&c,None,1000000).unwrap().is_empty());
    }

    #[test]
    fn activity_expires_without_database_writes() {
        let c=database();
        c.execute_batch("INSERT INTO thread_turns VALUES('chat','active','inProgress',100,NULL,1);
            INSERT INTO thread_items VALUES('chat','active',110000,'agentMessage');").unwrap();
        assert_eq!(desktop_codex_activity(&c,None,111000).unwrap().len(),1);
        assert!(desktop_codex_activity(&c,None,800000).unwrap().is_empty());
    }

    #[test]
    fn cached_rollout_keeps_the_step_specific_timeout() {
        let at=1_800_000_000_000;
        assert!(rollout_busy(Some((CodexStep::Thinking,at)),at+119000));
        assert!(!rollout_busy(Some((CodexStep::Thinking,at)),at+121000));
        assert!(rollout_busy(Some((CodexStep::AsstMsg,at)),at+3000));
        assert!(!rollout_busy(Some((CodexStep::AsstMsg,at)),at+5000));
        assert!(!rollout_busy(Some((CodexStep::Aborted,at)),at));
    }

    #[test]
    fn final_answer_is_idle_without_task_complete_event() {
        let raw=json!({"timestamp":"2026-10-20T10:00:00Z","type":"response_item",
            "payload":{"type":"message","role":"assistant","channel":"final","content":[]}}).to_string();
        let step=codex_last_step(&raw);
        assert_eq!(step.unwrap().0,CodexStep::Aborted);
        assert!(!rollout_busy(step,1_800_000_000_000));
    }

    #[test]
    fn completed_desktop_state_overrides_recent_transcript() {
        let c=database();
        c.execute_batch("INSERT INTO thread_turns VALUES('01a1552d-d1ca-7073-8ac9-4a8a4ee6292d','turn','completed',200,201,1);").unwrap();
        let path=std::path::Path::new("rollout-2026-10-20T13-00-00-01a1552d-d1ca-7073-8ac9-4a8a4ee6292d.jsonl");
        assert!(rollout_is_completed(path,Some(&c),Some((CodexStep::Tool,200000))));
        assert!(!rollout_is_completed(path,Some(&c),Some((CodexStep::Thinking,202000))));
    }
}
