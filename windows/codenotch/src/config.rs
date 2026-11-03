use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_port")]
    pub port: u16,
    /// "auto" | "zh" | "en" | "ja" | "ko"
    #[serde(default = "default_lang")]
    pub lang: String,
    #[serde(default)]
    pub bar_x: Option<i32>,
    #[serde(default)]
    pub bar_y: Option<i32>,
    /// Logical width of the bar (wheel-adjustable, 220-520); None = default 360
    #[serde(default)]
    pub bar_w: Option<u32>,
    /// Allow dragging + wheel resizing (tray toggle, off by default to prevent accidental drags)
    #[serde(default)]
    pub drag_enabled: bool,
    /// Vertical position of the notch: the window centre as a fraction of the target monitor's height (0 = top, 1 = bottom), default 0.5; saved after a drag
    #[serde(default = "default_notch_y")]
    pub notch_y: f64,
    /// Which monitor the notch is pinned to: the monitor's own name (e.g. "\\.\DISPLAY2"), or None for the primary
    #[serde(default)]
    pub monitor: Option<String>,
    /// Which screen edge it hugs: "right" | "left" (the pill mirrors, the hover card opens the other way)
    #[serde(default = "default_edge")]
    pub edge: String,
    /// DeepSeek API key (Bearer). Empty = the DeepSeek cell stays hidden. Kept out of the way in
    /// deepseek.json / DEEPSEEK_API_KEY is preferred; this field is the last resort.
    #[serde(default)]
    pub deepseek_key: String,
    /// Declared prepaid DeepSeek budget in the balance's own currency; > 0 turns the ring into a
    /// derived "budget used" percentage. 0 keeps the cell on the vendor's balance number.
    #[serde(default)]
    pub deepseek_budget: f64,
    /// Provider ids the user switched off ("codex", "cursor", "gemini", "deepseek", "kimi").
    /// A hidden provider draws no cell, stops polling, and stops being probed for activity — the
    /// point of hiding Cursor is not to keep opening its SQLite every few seconds.
    #[serde(default)]
    pub hidden: Vec<String>,
}

/// Ids the page and the tray agree on, in the order the pill draws them
pub const PROVIDERS: [(&str, &str); 5] = [
    ("codex", "Codex"),
    ("cursor", "Cursor"),
    ("gemini", "Antigravity"),
    ("deepseek", "DeepSeek"),
    ("kimi", "Kimi"),
];

/// Is this provider switched off? Read from disk, so a toggle takes effect without a restart
/// (the tray writes the config; the polling loops ask this once per tick).
#[cfg(test)]
pub fn hidden_in(cfg: &Config, id: &str) -> bool {
    cfg.hidden.iter().any(|h| h == id)
}

pub fn is_hidden(id: &str) -> bool {
    load().hidden.iter().any(|h| h == id)
}

fn default_edge() -> String {
    "right".into()
}

fn default_notch_y() -> f64 {
    0.5
}

fn default_port() -> u16 {
    48666
}
fn default_lang() -> String {
    "auto".into()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: default_port(),
            lang: default_lang(),
            bar_x: None,
            bar_y: None,
            bar_w: None,
            drag_enabled: false,
            notch_y: default_notch_y(),
            monitor: None,
            edge: default_edge(),
            deepseek_key: String::new(),
            deepseek_budget: 0.0,
            hidden: Vec::new(),
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("codenotch")
        .join("config.json")
}

pub fn load() -> Config {
    let path = config_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save(cfg: &Config) {
    let path = config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(txt) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(path, txt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hidden_provider_is_matched_by_id() {
        let cfg = Config { hidden: vec!["cursor".into(), "gemini".into()], ..Default::default() };
        assert!(hidden_in(&cfg, "cursor"));
        assert!(hidden_in(&cfg, "gemini"));
        assert!(!hidden_in(&cfg, "kimi"));
    }

    #[test]
    fn an_older_config_without_the_field_hides_nothing() {
        let cfg: Config = serde_json::from_str(r#"{"port":48666,"lang":"auto"}"#).unwrap();
        assert!(cfg.hidden.is_empty());
        assert!(!hidden_in(&cfg, "cursor"));
    }

    #[test]
    fn the_provider_table_matches_the_page_ids() {
        let ids: Vec<&str> = PROVIDERS.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec!["codex", "cursor", "gemini", "deepseek", "kimi"]);
    }
}
