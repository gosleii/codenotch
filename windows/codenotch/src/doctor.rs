//! `codenotch.exe doctor` — self-diagnosis: look instead of guessing.
//! Checks the config, port occupancy, every provider's credential/source probe, the provider
//! glyphs and the working-state probe, and writes to stdout plus %APPDATA%\codenotch\doctor.log.

pub fn run() -> String {
    let mut o = String::new();
    o += &format!("== Codenotch doctor v{} ==\n", env!("CARGO_PKG_VERSION"));

    let cfg = crate::config::load();
    o += &format!(
        "config: port={} lang={} edge={} monitor={} hidden={} ({})\n",
        cfg.port,
        cfg.lang,
        cfg.edge,
        cfg.monitor.clone().unwrap_or_else(|| "primary".into()),
        cfg.hidden.join(","),
        crate::config::config_path().display()
    );

    match std::net::TcpListener::bind(("127.0.0.1", cfg.port)) {
        Ok(_) => o += "port: free — no Codenotch instance is running\n",
        Err(_) => o += "port: in use — an instance is already running (quit it from the tray before starting a new build)\n",
    }

    o += &format!("\nusage sources:\n  {}\n  {}\n", crate::codex::probe(), crate::kimi::probe());
    o += &format!("  {}\n", crate::cursor::probe());
    o += &format!("  {}\n", crate::antigravity::probe());
    o += &format!("  {}\n", crate::deepseek::probe());
    o += &format!("\nprovider glyphs:\n{}\n", crate::glyphs::probe());
    o += &format!("\nworking state:\n  {}\n", crate::activity::probe());
    o
}
