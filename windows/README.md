# Codenotch for Windows

A Windows port of [Codenotch](https://github.com/vinzdg/codenotch) — the usage notch that
sits on the edge of your screen and answers two questions at a glance:
**how much of my AI allowance is left**, and **is Claude still working**.

Same design language as the macOS original (inverse-rounded pill, colour-graded rings,
hover card with per-window bars), rebuilt for Windows in Rust + Tauri 2 / WebView2.
No code is copied from the Swift app; the providers are reimplemented from their
documented behaviour and the wire formats.

## What it shows

| Cell | Source | How it reads it |
|---|---|---|
| **Claude** | `GET https://api.anthropic.com/api/oauth/usage` with the token Claude Code keeps in `~/.claude/.credentials.json` | Session / weekly windows, 429 back-off with a persisted deadline, stale readings dimmed with their age. A thin arc spins inside the ring while a Claude session is working, and pulses amber when one is waiting on you (Claude Code hooks + transcript watcher, desktop app included). |
| **Codex** | `GET https://chatgpt.com/backend-api/wham/usage` with the session Codex keeps in `~/.codex/auth.json` (read only, never refreshed), falling back to the `rate_limits` snapshot in the newest rollout log | Live primary/secondary windows (5h + weekly on paid plans, a monthly window on free) while Codex is signed in; otherwise the last snapshot, marked stale by its own timestamp. |
| **Cursor** | The editor's own session from `state.vscdb` → `cursor.com/api/usage-summary` | Included usage / API usage / on-demand, reset at billing-cycle end. Nothing to sign into: it borrows the editor's session, so there is only ever one account. |
| **Antigravity** | The local `language_server` bridge (quota summary), then Google's Cloud Code API for licensed accounts, then a plain count of today's model turns | Honest degradation: a percentage only when one exists, a `~count` when it does not. |
| **DeepSeek** | `GET https://api.deepseek.com/user/balance` with an API key you supply | DeepSeek is prepaid, so the account's own number is the balance: the cell prints it (`¥110.00`) and the ring keeps its bare track. Declare a budget and the ring becomes a "budget used" percentage — that denominator is ours, hence the `~`. |

Providers that are not installed simply do not get a cell. DeepSeek's cell appears only once a key exists.

### DeepSeek

There is no local session to borrow, so the key is yours. The app reads it from the first of:

1. `%APPDATA%\codenotch\deepseek.json` — `{ "api_key": "sk-…", "budget": 20.0 }`
2. the `DEEPSEEK_API_KEY` environment variable
3. `config.json` — `{ "deepseek_key": "sk-…", "deepseek_budget": 20.0 }`

`budget` is optional and is read in the balance's own currency; leave it out (or 0) to keep the cell on
the vendor's number. The key is never written by the app, never logged, and a rejected key only turns
the ring into a dash. No key → no cell.

To check a key without waiting for the notch, ask the app directly:

```powershell
.\target\release\codenotch.exe deepseek              # what is configured, and where the key is read from
.\target\release\codenotch.exe deepseek sk-…         # one live round trip, printed
```

The response shape is pinned by `deepseek.rs`'s tests against the example published at
[api-docs.deepseek.com/api/get-user-balance](https://api-docs.deepseek.com/api/get-user-balance).
A 401 means the host is reachable and the key was not accepted; DeepSeek answers 401 for an unknown
path as well, so it says nothing about the path either way.

## Where it sits

The notch is pinned to the **right edge of the primary monitor** by default. The tray's **Screen** and
**Edge** menus move it: pick any attached monitor by name, and either its right or its left edge. On a
left edge the whole surface mirrors — the pill's rounded side and fillets swap over and the hover card
opens to the right. The choice lives in `config.json` (`monitor`, `edge`, `notch_y`) and survives a
restart; a monitor that is no longer attached falls back to the primary, so the notch can never end up
off-screen.

## Install / build

Prerequisites: Rust (MSVC toolchain), WebView2 runtime (ships with Windows 11).

```powershell
# from this directory (the repo root here; `windows/` inside the upstream repo)
cargo build --release
.\target\release\codenotch.exe          # pill appears on the right edge of the primary monitor
.\target\release\codenotch.exe doctor   # self-diagnosis: credentials, data sources, icons, hooks
```

Tray menu: refresh now, reset position, open data folder (`%APPDATA%\codenotch` — logs,
persisted readings, icon overrides), start with Windows, install/uninstall Claude Code hooks.

### Icons

Provider marks are the SVGs from [`@lobehub/icons-static-svg`](https://github.com/lobehub/lobe-icons)
(MIT), embedded unmodified — see `codenotch/glyphs/NOTICE.md`. Drop your own
`claude|codex|cursor|gemini.svg` (or `.png`) into `%APPDATA%\codenotch\glyphs\` to override.
The marks remain the trademarks of their owners.

## Layout

```
.
├── codenotch/          Tauri 2 app: window, tray, providers (usage.rs, codex.rs, cursor.rs, antigravity.rs),
│   ├── src/            session engine (watcher.rs, state.rs, focus.rs), glyphs.rs, doctor.rs
│   ├── ui/notch.html   the pill + hover card (single file, no framework)
│   └── glyphs/         provider marks (+ NOTICE.md)
└── codenotch-hook/     <5 ms hook messenger Claude Code calls; forwards events to the app
```

## Relationship to upstream

This port follows the upstream design spec (`docs/specs/2026-08-28-usage-notch-design.md`)
and provider semantics. It is developed at
[Im-Midi/codenotch-windows](https://github.com/Im-Midi/codenotch-windows) and offered to the
upstream project as its `windows/` tree; the two are kept in sync. The session-detection engine
originated in [Im-Midi/Pac-Man](https://github.com/Im-Midi/Pac-Man) (MIT).

## License

MIT — see `LICENSE`. The Codenotch design and name belong to the upstream author.
