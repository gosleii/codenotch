//! Rust-side (tray menu) strings. The page has its own dictionary; keys are kept identical on both sides.

pub fn resolve_auto() -> &'static str {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Globalization::GetUserDefaultLocaleName;
        let mut buf = [0u16; 85];
        let n = GetUserDefaultLocaleName(&mut buf);
        if n > 0 {
            let name = String::from_utf16_lossy(&buf[..(n as usize - 1)]).to_lowercase();
            if name.starts_with("zh") {
                return "zh";
            }
            if name.starts_with("ja") {
                return "ja";
            }
            if name.starts_with("ko") {
                return "ko";
            }
        }
    }
    "en"
}

pub fn tr(lang: &str, key: &str) -> &'static str {
    let l = if lang == "auto" { resolve_auto() } else { lang };
    match (l, key) {
        ("zh", "language") => "语言",
        ("zh", "lang_auto") => "跟随系统",
        ("zh", "reset_pos") => "重置悬浮条位置",
        ("zh", "quit") => "退出",
        ("zh", "autostart") => "开机自启（静默待命）",
        ("ja", "autostart") => "Windows起動時に自動開始",
        ("ko", "autostart") => "Windows 시작 시 자동 실행",
        ("zh", "refresh") => "立即刷新用量",
        ("zh", "open_data") => "打开数据文件夹（日志 / 图标）",
        ("ja", "open_data") => "データフォルダを開く（ログ / アイコン）",
        ("ko", "open_data") => "데이터 폴더 열기 (로그 / 아이콘)",
        (_, "open_data") => "Open data folder (logs / icons)",
        ("ja", "refresh") => "使用量を今すぐ更新",
        ("ko", "refresh") => "사용량 지금 새로고침",
        ("ja", "language") => "言語",
        ("ja", "lang_auto") => "システムに従う",
        ("ja", "reset_pos") => "バー位置をリセット",
        ("ja", "quit") => "終了",
        ("ko", "language") => "언어",
        ("ko", "lang_auto") => "시스템 따르기",
        ("ko", "reset_pos") => "바 위치 초기화",
        ("ko", "quit") => "종료",
        (_, "language") => "Language",
        (_, "lang_auto") => "Follow system",
        (_, "reset_pos") => "Reset bar position",
        (_, "quit") => "Quit",
        (_, "autostart") => "Start with Windows (silent)",
        (_, "refresh") => "Refresh usage now",
        ("zh", "screen") => "屏幕",
        ("zh", "edge") => "贴边位置",
        ("zh", "edge_right") => "右边缘",
        ("zh", "edge_left") => "左边缘",
        ("zh", "monitor_primary") => "主屏幕",
        ("zh", "monitor_primary_tag") => "主屏",
        ("ja", "screen") => "画面",
        ("ja", "edge") => "貼り付ける辺",
        ("ja", "edge_right") => "右端",
        ("ja", "edge_left") => "左端",
        ("ja", "monitor_primary") => "メインモニター",
        ("ja", "monitor_primary_tag") => "メイン",
        ("ko", "screen") => "화면",
        ("ko", "edge") => "붙일 모서리",
        ("ko", "edge_right") => "오른쪽 가장자리",
        ("ko", "edge_left") => "왼쪽 가장자리",
        ("ko", "monitor_primary") => "기본 모니터",
        ("ko", "monitor_primary_tag") => "기본",
        (_, "screen") => "Screen",
        (_, "edge") => "Edge",
        (_, "edge_right") => "Right edge",
        (_, "edge_left") => "Left edge",
        (_, "monitor_primary") => "Primary monitor",
        (_, "monitor_primary_tag") => "primary",
        ("zh", "providers") => "提供商",
        ("ja", "providers") => "プロバイダー",
        ("ko", "providers") => "공급자",
        (_, "providers") => "Providers",
        _ => "?",
    }
}
