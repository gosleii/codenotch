use crate::hooks_install;
use crate::i18n::tr;
use tauri::menu::{CheckMenuItemBuilder, Menu, MenuBuilder, MenuItemBuilder, SubmenuBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, Wry};

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let lang = {
        let st = app.state::<crate::AppState>();
        let c = st.cfg.lock().unwrap();
        c.lang.clone()
    };
    let menu = build_menu(app, &lang)?;
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    TrayIconBuilder::with_id("main")
        .icon(icon)
        .tooltip(concat!("Codenotch v", env!("CARGO_PKG_VERSION")))
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, ev| handle(app, ev.id().as_ref()))
        .build(app)?;
    Ok(())
}

pub fn build_menu(app: &AppHandle, lang: &str) -> tauri::Result<Menu<Wry>> {
    let install = MenuItemBuilder::with_id("install", tr(lang, "install")).build(app)?;
    let uninstall = MenuItemBuilder::with_id("uninstall", tr(lang, "uninstall")).build(app)?;
    let l_auto = CheckMenuItemBuilder::with_id("lang-auto", tr(lang, "lang_auto"))
        .checked(lang == "auto")
        .build(app)?;
    let l_zh = CheckMenuItemBuilder::with_id("lang-zh", "中文")
        .checked(lang == "zh")
        .build(app)?;
    let l_en = CheckMenuItemBuilder::with_id("lang-en", "English")
        .checked(lang == "en")
        .build(app)?;
    let l_ja = CheckMenuItemBuilder::with_id("lang-ja", "日本語")
        .checked(lang == "ja")
        .build(app)?;
    let l_ko = CheckMenuItemBuilder::with_id("lang-ko", "한국어")
        .checked(lang == "ko")
        .build(app)?;
    let lang_menu = SubmenuBuilder::new(app, tr(lang, "language"))
        .items(&[&l_auto, &l_zh, &l_en, &l_ja, &l_ko])
        .build()?;
    let refresh = MenuItemBuilder::with_id("refresh", tr(lang, "refresh")).build(app)?;
    let reset = MenuItemBuilder::with_id("reset", tr(lang, "reset_pos")).build(app)?;
    // Screen: one check item per attached monitor, plus "Primary" for the default. The id carries
    // the monitor's own name, which is what the config stores and what place_notch looks up.
    let current_monitor = {
        let st = app.state::<crate::AppState>();
        let c = st.cfg.lock().unwrap();
        c.monitor.clone()
    };
    let primary_item = CheckMenuItemBuilder::with_id("mon-", tr(lang, "monitor_primary"))
        .checked(current_monitor.is_none())
        .build(app)?;
    let mut screen_menu = SubmenuBuilder::new(app, tr(lang, "screen")).item(&primary_item);
    for (name, is_primary) in crate::monitor_list(app) {
        let label = if is_primary { format!("{name} · {}", tr(lang, "monitor_primary_tag")) } else { name.clone() };
        let item = CheckMenuItemBuilder::with_id(format!("mon-{name}"), label)
            .checked(current_monitor.as_deref() == Some(name.as_str()))
            .build(app)?;
        screen_menu = screen_menu.item(&item);
    }
    let screen_menu = screen_menu.build()?;
    let current_edge = {
        let st = app.state::<crate::AppState>();
        let c = st.cfg.lock().unwrap();
        c.edge.clone()
    };
    let e_right = CheckMenuItemBuilder::with_id("edge-right", tr(lang, "edge_right"))
        .checked(current_edge != "left")
        .build(app)?;
    let e_left = CheckMenuItemBuilder::with_id("edge-left", tr(lang, "edge_left"))
        .checked(current_edge == "left")
        .build(app)?;
    let edge_menu = SubmenuBuilder::new(app, tr(lang, "edge"))
        .items(&[&e_right, &e_left])
        .build()?;
    // Providers: a check item means "shown". Unchecking one removes its cell and stops its polling
    // and its activity probe, so a provider you do not use costs nothing while it is off.
    let hidden = {
        let st = app.state::<crate::AppState>();
        let c = st.cfg.lock().unwrap();
        c.hidden.clone()
    };
    let mut providers_menu = SubmenuBuilder::new(app, tr(lang, "providers"));
    for (id, label) in crate::config::PROVIDERS {
        let item = CheckMenuItemBuilder::with_id(format!("prov-{id}"), label)
            .checked(!hidden.iter().any(|h| h == id))
            .build(app)?;
        providers_menu = providers_menu.item(&item);
    }
    let providers_menu = providers_menu.build()?;
    let open_data = MenuItemBuilder::with_id("open-data", tr(lang, "open_data")).build(app)?;
    let auto = CheckMenuItemBuilder::with_id("autostart", tr(lang, "autostart"))
        .checked(crate::autostart::is_enabled())
        .build(app)?;
    let quit = MenuItemBuilder::with_id("quit", tr(lang, "quit")).build(app)?;
    MenuBuilder::new(app)
        .items(&[&install, &uninstall])
        .separator()
        .item(&lang_menu)
        .item(&refresh)
        .item(&providers_menu)
        .item(&screen_menu)
        .item(&edge_menu)
        .item(&reset)
        .item(&open_data)
        .item(&auto)
        .separator()
        .item(&quit)
        .build()
}

fn refresh_menu(app: &AppHandle) {
    let lang = {
        let st = app.state::<crate::AppState>();
        let c = st.cfg.lock().unwrap();
        c.lang.clone()
    };
    if let Some(tray) = app.tray_by_id("main") {
        if let Ok(menu) = build_menu(app, &lang) {
            let _ = tray.set_menu(Some(menu));
        }
    }
}

fn handle(app: &AppHandle, id: &str) {
    match id {
        "install" => notice(app, hooks_install::install()),
        "uninstall" => notice(app, hooks_install::uninstall()),
        "reset" => crate::reset_bar(app),
        "open-data" => {
            let dir = crate::config::config_path().parent().map(|p| p.to_path_buf()).unwrap_or_default();
            let _ = std::fs::create_dir_all(crate::glyphs::user_dir());
            let mut cmd = std::process::Command::new("explorer");
            cmd.arg(dir.as_os_str());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000);
            }
            let _ = cmd.spawn();
        }
        "refresh" => {
            {
                let st = app.state::<crate::AppState>();
                let mut u = st.usage.lock().unwrap();
                u.backoff_until = 0;
            }
            crate::usage::request_refresh();
            crate::codex::request_refresh();
            crate::cursor::request_refresh();
            crate::antigravity::request_refresh();
            crate::deepseek::request_refresh();
            let a = app.clone();
            std::thread::spawn(move || crate::reload_glyphs(&a));
        }
        "autostart" => {
            let r = if crate::autostart::is_enabled() {
                crate::autostart::disable()
            } else {
                crate::autostart::enable()
            };
            notice(app, r);
            refresh_menu(app); // refresh the check marks
        }
        "quit" => app.exit(0),
        _ if id.starts_with("lang-") => crate::apply_lang(app, &id[5..]),
        // "mon-" with an empty remainder is the primary monitor; otherwise the name follows verbatim
        _ if id.starts_with("mon-") => {
            {
                let st = app.state::<crate::AppState>();
                let mut c = st.cfg.lock().unwrap();
                c.monitor = Some(id[4..].to_string()).filter(|n| !n.is_empty());
                crate::config::save(&c);
            }
            crate::place_notch(app);
            refresh_menu(app);
        }
        _ if id.starts_with("prov-") => {
            let pid = id[5..].to_string();
            {
                let st = app.state::<crate::AppState>();
                let mut c = st.cfg.lock().unwrap();
                match c.hidden.iter().position(|h| *h == pid) {
                    Some(pos) => {
                        c.hidden.remove(pos);
                    }
                    None => c.hidden.push(pid),
                }
                crate::config::save(&c);
            }
            crate::broadcast_providers(app);
            refresh_menu(app);
        }
        _ if id.starts_with("edge-") => {
            {
                let st = app.state::<crate::AppState>();
                let mut c = st.cfg.lock().unwrap();
                c.edge = id[5..].to_string();
                crate::config::save(&c);
            }
            crate::place_notch(app);
            refresh_menu(app);
        }
        _ => {}
    }
}

fn notice(app: &AppHandle, r: Result<String, String>) {
    let msg = match r {
        Ok(m) => m,
        Err(e) => format!("Error: {e}"),
    };
    let _ = app.emit("notice", &msg);
}
