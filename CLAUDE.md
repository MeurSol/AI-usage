# AI-usage

A macOS menu bar app (Rust) that shows Claude usage in the system status bar:
the current **session (5h)** and **weekly (7d)** utilization, plus when each
limit resets. The bar shows `session% / weekly%` (e.g. `27% / 18%`); the
dropdown lists each window with its reset time, and a Quit item.

## Architecture

```
src/
  main.rs              NSApplication (accessory) bootstrap + run loop
  menubar.rs           AppKit status item + dropdown; 1s NSTimer redraws from state
  poller.rs            background thread polls a Provider into Arc<Mutex<AppState>>
  keychain.rs          read the Claude OAuth token from the macOS Keychain
  provider/
    mod.rs             Provider trait + normalized UsageWindow / UsageSnapshot / FetchError
    claude.rs          ClaudeProvider: keychain -> HTTP GET -> parse
```

Threading: the worker thread does all I/O and writes `AppState`; the main
thread only reads it (AppKit must be touched on the main thread only).

## Data source

The official usage data is fetched the same way Claude Code's `/usage` does —
no log scraping:

- **Token**: macOS Keychain generic-password, service `Claude Code-credentials`,
  account = login short name. JSON blob → `claudeAiOauth.accessToken`.
- **Endpoint**: `GET https://api.anthropic.com/api/oauth/usage`
  - headers: `Authorization: Bearer <token>`, `anthropic-beta: oauth-2025-04-20`
  - response: `five_hour` (session) and `seven_day` (weekly), each
    `{ utilization: f64, resets_at: RFC3339 }`. Other fields
    (`seven_day_opus`, `extra_usage`, …) are currently ignored.

## Build & run

```sh
cargo run            # dev; status item appears top-right
cargo test           # parse unit test
cargo build --release
```

Requires Rust ≥ 1.85 (deps use edition 2024). First run triggers a one-time
**Keychain access prompt** — choose "Always Allow" to silence future reads.

### Proxy

`api.anthropic.com` returns **403 on direct access in some regions**; it must
be reached through an HTTP proxy. The app uses the first `http://` proxy from
`HTTPS_PROXY` / `HTTP_PROXY` (it ignores `ALL_PROXY`, which is often socks5).
Launch from a shell where these are set (e.g. `http://127.0.0.1:7890`).
Note: a GUI `.app` launched from Finder won't inherit shell env — see Follow-ups.

## Extending (new providers)

The `Provider` trait (`provider/mod.rs`) is the single extension point. To add
a source (Anthropic API usage, Codex, …):

1. Add `provider/<name>.rs` implementing `Provider::fetch() -> Result<UsageSnapshot, FetchError>`.
2. Normalize its data into `UsageWindow { label, utilization, resets_at }`.
3. Wire it in `main.rs` (today a single `ClaudeProvider` is polled).

The UI and poller are provider-agnostic and need no changes.

## Conventions

- Follow `andrej-karpathy-guidelines`: minimal code, no speculative
  abstraction (the one trait is the requested extension point), surgical edits.
- Never log or persist the OAuth token. It is read from the Keychain at runtime
  only and re-read each poll (Claude Code refreshes it in place).
- Conventional commits; work on a feature branch.

## Follow-ups (not yet built)

- `.app` bundle + launch-at-login (and proxy config for GUI launch context).
- OAuth token auto-refresh on expiry (currently shows `auth?` → re-login in
  Claude Code).
- Opus/Sonnet weekly breakdown + `extra_usage` display.
- API usage / Codex providers.
