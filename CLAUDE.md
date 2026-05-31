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
  poller.rs            worker thread polls a Provider into Arc<Mutex<AppState>>
  watch.rs             notify watcher on ~/.claude/projects to refresh per turn
  token.rs             OAuth access-token cache + refresh-token rotation
  keychain.rs          read/write the Claude OAuth credentials in the Keychain
  provider/
    mod.rs             Provider trait + normalized UsageWindow / UsageSnapshot / FetchError
    claude.rs          ClaudeProvider: keychain -> HTTP GET -> parse
packaging/Info.plist   .app bundle metadata (LSUIElement agent app)
scripts/install.sh     build + bundle + register login LaunchAgent
scripts/uninstall.sh   remove agent + .app
```

Threading: the worker thread does all I/O and writes `AppState`; the main
thread only reads it (AppKit must be touched on the main thread only).

### Refresh timing (`poller.rs`)

The worker fetches, then sleeps until the soonest of several triggers
(`next_wait` picks the interval):

- **Turn event** — `watch.rs` signals on `*.jsonl` writes under
  `~/.claude/projects` (a conversation turn); 800ms debounce.
- **Menu open** — the `NSMenuDelegate` (`menubar.rs`) fires a `Trigger` so the
  numbers are fresh the moment the dropdown opens.
- **Reset boundary** — wakes ~3s after the soonest window's `resets_at`, so the
  bar updates at a reset even with no conversation active.
- **Heartbeat** — 60s ceiling so it stays current regardless.

Trigger-driven fetches are rate-limited to one per `MIN_TRIGGER_GAP` (5s) so
rapid menu opens / turn bursts coalesce and don't hit the endpoint's 429.

## Data source

The official usage data is fetched the same way Claude Code's `/usage` does —
no log scraping:

- **Token**: macOS Keychain generic-password, service `Claude Code-credentials`,
  account = login short name. JSON blob → `claudeAiOauth.{accessToken,
  refreshToken, expiresAt}`.
- **Endpoint**: `GET https://api.anthropic.com/api/oauth/usage`
  - headers: `Authorization: Bearer <token>`, `anthropic-beta: oauth-2025-04-20`
  - response: `five_hour` (session) and `seven_day` (weekly), each
    `{ utilization: f64, resets_at: RFC3339 }`. Other fields
    (`seven_day_opus`, `extra_usage`, …) are currently ignored.
  - returns **429** if polled too aggressively — handled as a transient error
    (keeps the last snapshot, retries next tick).

### Token auto-refresh (`token.rs`)

When the Keychain access token is within 60s of `expiresAt` (or a usage call
returns 401), `TokenManager` refreshes it:
`POST https://platform.claude.com/v1/oauth/token` with
`{ grant_type: "refresh_token", refresh_token, client_id }` (Claude Code's
client_id). The rotated `access_token` / `refresh_token` are **written back to
the Keychain** (preserving all other fields) so Claude Code stays in sync, and
cached in memory. Only if the refresh itself fails does the bar show `auth?`.

## Build & run

```sh
cargo run            # dev; status item appears top-right
cargo test           # parse unit test
cargo build --release
```

Requires Rust ≥ 1.85 (deps use edition 2024). First run may trigger a one-time
**Keychain access prompt** — choose "Always Allow" to silence future reads.

### Install / launch at login

```sh
scripts/install.sh     # build, install ~/Applications/AI-usage.app, register LaunchAgent
scripts/uninstall.sh   # remove it
```

`install.sh` assembles the `.app` (using `packaging/Info.plist`) and writes a
LaunchAgent at `~/Library/LaunchAgents/com.machine.ai-usage.plist` with
`RunAtLoad` so it starts at login, then loads it via
`launchctl bootstrap`/`kickstart`. Re-run to update. The script bakes the
resolved proxy (below) into the agent's `EnvironmentVariables` so it works in
the login context. Quit (from the menu) stays quit until next login.

### Proxy

`api.anthropic.com` returns **403 on direct access in some regions**; it must
be reached through an HTTP proxy. Resolution order (`provider/claude.rs`):
1. `HTTPS_PROXY` / `HTTP_PROXY` (http scheme; `ALL_PROXY`/socks5 ignored).
2. macOS system proxy via `scutil --proxy` — so a Finder/login-launched `.app`
   works even without shell env.

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

- Opus/Sonnet weekly breakdown + `extra_usage` display.
- API usage / Codex providers.
- Code-sign the bundle (unsigned binaries may re-prompt for Keychain access
  after each rebuild).
