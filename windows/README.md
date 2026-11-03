# Codenotch for Windows

A Windows port of [Codenotch](https://github.com/vinzdg/codenotch) — the usage notch that
sits on the edge of your screen and answers one question at a glance:
**how much of my AI allowance is left.**

Same design language as the macOS original (inverse-rounded pill, colour-graded rings,
hover card with per-window bars), rebuilt for Windows in Rust + Tauri 2 / WebView2.
No code is copied from the Swift app; the providers are reimplemented from their
documented behaviour and the wire formats.

## What it shows

| Cell | Source | How it reads it |
|---|---|---|
| **Codex** | Windows WinHTTP with system TLS/proxy/PAC settings → `GET https://chatgpt.com/backend-api/wham/usage`, borrowing `~/.codex/auth.json` read only | Live primary/secondary windows. On failure, keeps whichever is newer: the last successful reading or the newest rollout snapshot, marked stale. Expired windows are removed rather than presented as current. Retries failures after 30 s, respecting persisted 429 `Retry-After`; successful polling stays at 5 min. |
| **Kimi** | The Kimi Code local server: `GET http://127.0.0.1:<port>/api/v1/oauth/usage` with `~/.kimi-code/server.token` | The 5-hour, monthly-code and monthly-total windows (`usedRatio` + `resetAt`). The port comes from the freshest live `~/.kimi-code/server/instances/*.json` — no cell while Kimi Code is not running. Read-only; the token never leaves 127.0.0.1. |
| **Cursor** | The editor's own session from `state.vscdb` → `cursor.com/api/usage-summary` | Included usage / API usage / on-demand, reset at billing-cycle end. Nothing to sign into: it borrows the editor's session, so there is only ever one account. |
| **Antigravity** | The local `language_server` bridge (quota summary), then Google's Cloud Code API for licensed accounts, then a plain count of today's model turns | Honest degradation: a percentage only when one exists, a `~count` when it does not. |
| **DeepSeek** | `GET https://api.deepseek.com/user/balance` with an API key you supply | DeepSeek is prepaid, so the account's own number is the balance: the cell prints it (`¥110.00`) and the ring keeps its bare track. Declare a budget and the ring becomes a "budget used" percentage — that denominator is ours, hence the `~`. |

Providers that are not installed (or not running, for Kimi) simply do not get a cell.
DeepSeek's cell appears only once a key exists.

### Kimi

Kimi Code keeps its OAuth session and its quota behind a local server (`kimi web`, or the
desktop app). Codenotch borrows it: the newest instance file under
`~/.kimi-code/server/instances/` that is still heartbeating gives the host and port, and
`~/.kimi-code/server.token` authenticates. Nothing is written back. To check the reading
without waiting for the notch, ask the app directly:

```powershell
.\target\release\codenotch.exe kimi     # one live round trip, printed
```

### DeepSeek

There is no local session to borrow, so the key is yours. The app reads it from the first of:

1. `%APPDATA%\codenotch\deepseek.json` — `{ "api_key": "sk-…", "budget": 20.0 }`
2. `DEEPSEEK_API_KEY` in the app's own environment
3. `DEEPSEEK_API_KEY` in the **user** environment (`HKCU\Environment`) — where `setx` puts it, and
   the same value the DSH harness resolves for its own balance widget, so a key already set up for
   DSH needs no second copy here. A process only inherits the environment it was started with, so
   the registry is read directly rather than trusting `std::env`.
4. `config.json` — `{ "deepseek_key": "sk-…", "deepseek_budget": 20.0 }`

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
off-screen. Whatever is chosen is also where it comes back at sign-in, since the position is read from
the same config.

## Install / build

Prerequisites: Rust (MSVC toolchain), WebView2 runtime (ships with Windows 11).

```powershell
# from this directory (the repo root here; `windows/` inside the upstream repo)
cargo build --release
.\target\release\codenotch.exe          # pill appears on the right edge of the primary monitor
.\target\release\codenotch.exe doctor   # self-diagnosis: credentials, data sources, icons, hooks
.\target\release\codenotch.exe codex    # one read-only usage check; prints no credentials
```

`GET http://127.0.0.1:48666/status` (or the configured local port) reports the running build,
the Codex and Kimi usage snapshots and the detected activity for diagnosing the installed
instance. It excludes conversation content and credentials.

### Install, and start with Windows

Pointing an autostart entry at `target\release` is asking for trouble: a `cargo clean`, or a rebuild
while the app is running, leaves the entry aiming at nothing. `install.ps1` copies the release binary
to `%LOCALAPPDATA%\Programs\Codenotch` and registers *that* copy:

```powershell
.\install.ps1                 # build, install, start with Windows, launch
.\install.ps1 -NoAutostart    # build, install, launch
.\install.ps1 -Uninstall      # remove the Run entry and the installed copy
```

The entry is a per-user `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value named
`Codenotch`, so it needs no administrator and can be turned off from the tray (**Start with Windows**)
or with `codenotch.exe autostart off`. The command line carries `--silent`, which this port accepts
and ignores: the notch appears as usual at sign-in.

Tray menu: refresh now, reset position, open data folder (`%APPDATA%\codenotch` — logs,
persisted readings, icon overrides), start with Windows.

### Icons

Provider marks are the SVGs from [`@lobehub/icons-static-svg`](https://github.com/lobehub/lobe-icons)
(MIT), embedded unmodified — see `codenotch/glyphs/NOTICE.md`. Drop your own
`codex|cursor|gemini|kimi.svg` (or `.png`) into `%APPDATA%\codenotch\glyphs\` to override.
The marks remain the trademarks of their owners.

## Layout

```
.
├── codenotch/          Tauri 2 app: window, tray, providers (usage.rs, codex.rs, cursor.rs,
│   ├── src/            antigravity.rs, deepseek.rs, kimi.rs), activity probe, glyphs.rs, doctor.rs
│   ├── ui/notch.html   the pill + hover card (single file, no framework)
│   └── glyphs/         provider marks (+ NOTICE.md)
└── install.ps1         build, install, autostart entry
```

## Relationship to upstream

This port follows the upstream design spec (`docs/specs/2026-08-28-usage-notch-design.md`)
and provider semantics. It is developed at
[Im-Midi/codenotch-windows](https://github.com/Im-Midi/codenotch-windows) and offered to the
upstream project as its `windows/` tree; the two are kept in sync. The session-detection engine
originated in [Im-Midi/Pac-Man](https://github.com/Im-Midi/Pac-Man) (MIT).

## License

MIT — see `LICENSE`. The Codenotch design and name belong to the upstream author.
