//! Shared usage-reading types. Every provider adapter (codex, cursor, antigravity, deepseek, kimi)
//! maps its vendor reply onto these, and the page renders them the same way.
//!
//! UsageSnapshot.status: ok | stale | needsAuth | backoff | error | absent
//!   - absent: the provider is not there at all (no credential, no live local server) — the page
//!     draws no cell for it
//!   - stale: a previous reading kept visible, with its age, after a failure — never invent a number

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LimitWindow {
    pub id: String,
    pub label: String,
    /// 0.0–1.0 (fraction used)
    pub used: f64,
    /// Reset time, ms epoch (None = unknown)
    pub resets_at: Option<u64>,
    /// Pure count window (no published denominator, e.g. Antigravity's requests today) — the cell shows ~N and the ring draws only its track
    #[serde(default)]
    pub count: Option<i64>,
    /// The number is ours, not the vendor's (upstream fidelity=.derived) — the card adds a ~ prefix
    #[serde(default)]
    pub derived: bool,
    /// A plain readout with no denominator of its own (DeepSeek's prepaid balance, say): the cell
    /// shows this text instead of a percentage and the ring draws only its track, exactly as a
    /// pure-count window does.
    #[serde(default)]
    pub text: Option<String>,
}

impl LimitWindow {
    /// Nothing to draw an arc from: a vendor count or a plain readout (DeepSeek's balance) is the
    /// whole reading, so the ring keeps its bare track and the cell prints the value instead.
    pub fn only_a_readout(&self) -> bool {
        self.count.is_some() || self.text.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageSnapshot {
    /// ok | stale | needsAuth | backoff | error | absent
    pub status: String,
    pub windows: Vec<LimitWindow>,
    pub fetched_at: u64,
    pub note: String,
    #[serde(default)]
    pub backoff_until: u64,
}
