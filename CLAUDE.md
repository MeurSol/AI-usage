# AI-usage

A macOS menu bar app (Rust) that shows GPT/Codex and Claude usage: the current
**session (5h)** and **weekly (7d)** utilization, plus when each limit resets.
The bar names the provider with the highest current session use (for example,
`GPT 42%`); the dropdown lists both providers and their windows, a **Last
refresh** line, a **Refresh now** item, and a Quit item.

## Architecture

```
src/
  main.rs              NSApplication (accessory) bootstrap + run loop
  menubar.rs           AppKit status item + dropdown; ~10fps NSTimer: spinner while fetching, else version-gated redraw
  gauge.rs             draws the session % pie as a colored NSImage (green→red ramp)
  poller.rs            independent provider workers + throttle/backoff policy
  watch.rs             routes Claude/Codex JSONL changes to matching workers
  token.rs             OAuth access-token cache + refresh-token rotation
  keychain.rs          read/write the Claude OAuth credentials in the Keychain
  provider/
    mod.rs             Provider trait + normalized UsageWindow / UsageSnapshot / FetchError
    claude.rs          ClaudeProvider: keychain -> HTTP GET -> parse
    gpt.rs             GptProvider: latest Codex token_count rate limits -> parse
packaging/Info.plist   .app bundle metadata (LSUIElement agent app, CFBundleIconFile)
packaging/AppIcon.icns  app icon (generated; committed)
scripts/render_icon.swift one-shot Swift renderer for the icon images
scripts/make-icon.sh   render_icon.swift → packaging/AppIcon.icns (iconutil)
scripts/setup-signing.sh  one-time: create the self-signed signing identity
scripts/install.sh     build + sign + bundle + icon + register login LaunchAgent
scripts/uninstall.sh   remove agent + .app
```

The status bar shows a small **session pie gauge** (`gauge.rs`) to the left of
the highest provider's `name session%` text. The fill is tinted by a green→red ramp
(`level_color`) so the level reads at a glance; a neutral gray track ring stays
visible on light and dark menu bars. The same ramp at 40% drives the app icon.

Threading: each provider owns a worker thread and writes only its section of
`AppState`; the main thread only reads it (AppKit must be touched on the main
thread only). GPT local reads therefore never wait for Claude's network I/O.
`AppState.version` is bumped on every state update; the UI timer (~10fps, to
animate the spinner) skips the redraw (rebuilding the gauge image + menu) when
the version is unchanged and no fetch is in flight, so idle ticks are cheap.
While any `ProviderState.refreshing` is true, the timer instead spins a small
indicator in the gauge slot (`gauge::spinner`).

### Refresh timing (`poller.rs`)

Each provider fetches only when it has a reason. Its worker sleeps until its own
next event or timer:

- **Targeted turn event** — Claude JSONL writes wake only Claude; Codex JSONL
  writes wake only GPT. Claude uses a 1.2s trailing debounce, GPT 250ms.
- **Per-provider request floor** — Claude requests remain at least 15s apart;
  local GPT reads remain at least 500ms apart. Bursts coalesce behind the floor.
- **Manual refresh** — **Refresh now** broadcasts to both workers but cannot
  bypass Claude's minimum gap or an active 429 cooldown.
- **Reset boundary** — each worker wakes ~3s after its own soonest `resets_at`.
- **Recovery** — normal failures back off from 30s to 5m. A Claude 429 has a
  dedicated 60s → 120s → 240s exponential cooldown capped at 15m; conversation
  and manual events queue behind it instead of defeating the backoff.
- **Sparse safety recheck** — a healthy provider with no reset timestamp checks
  every 6h, mainly to recover from a missed filesystem event.

The dropdown reports separate completion times for GPT and Claude, so an update
to one provider is never presented as if both were freshly checked.

## Data sources

Claude usage is fetched the same way Claude Code's `/usage` does:

- **Token**: macOS Keychain generic-password, service `Claude Code-credentials`,
  account = login short name. JSON blob → `claudeAiOauth.{accessToken,
  refreshToken, expiresAt}`.
- **Endpoint**: `GET https://api.anthropic.com/api/oauth/usage`
  - headers: `Authorization: Bearer <token>`, `anthropic-beta: oauth-2025-04-20`
  - response: `five_hour` (session) and `seven_day` (weekly), each
    `{ utilization: f64, resets_at: RFC3339 }`. Other fields
  (`seven_day_opus`, `extra_usage`, …) are currently ignored.
  - returns **429** if polled too aggressively — kept distinct from ordinary
    failures so the Claude worker preserves its snapshot and enters the longer
    non-bypassable exponential cooldown described above.

GPT usage comes from the newest `event_msg` / `token_count` event in the most
recent Codex JSONL files under `~/.codex/sessions`. Its `rate_limits.primary`
is the 300-minute session window and `secondary` is the 10,080-minute weekly
window. The provider reads only a bounded tail of recent logs and never reads
OpenAI credentials. An expired local window is treated as 0% until Codex writes
a newer server snapshot.

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
scripts/setup-signing.sh   # once: self-signed identity (stable Keychain grant)
scripts/install.sh         # build, sign, install /Applications/AI-usage.app, register LaunchAgent
scripts/uninstall.sh       # remove it
```

`install.sh` installs to `/Applications` (falls back to `~/Applications` if
that isn't writable), copies `AppIcon.icns` into the bundle's Resources, and
runs `lsregister` so the app + icon show in Finder / Launchpad / Spotlight.
Regenerate the icon with `scripts/make-icon.sh` after editing
`render_icon.swift`.

### Code signing

`setup-signing.sh` creates a self-signed code-signing identity `AI-usage Local`
in the login keychain (openssl `-legacy` PKCS12 so macOS `security` imports it).
It is **untrusted for Gatekeeper** (we launch the app directly, so that's fine)
and therefore does not appear under `security find-identity -v`; match it
without `-v`. `install.sh` signs the bundle with it using a fixed
`--identifier com.machine.ai-usage`, so the designated requirement
(`identifier … and certificate leaf = H"…"`) is **stable across rebuilds** —
which keeps the Keychain "Always Allow" grant for the OAuth token. Without the
identity, `install.sh` falls back to ad-hoc (grant resets each rebuild).

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
3. Wire it into the provider vector in `main.rs`.

The UI and poller are provider-agnostic and need no changes.

## Conventions

- Follow `andrej-karpathy-guidelines`: minimal code, no speculative
  abstraction (the one trait is the requested extension point), surgical edits.
- Never log or persist the OAuth token. It is read from the Keychain at runtime
  only and re-read each poll (Claude Code refreshes it in place).
- Conventional commits; work on a feature branch.

## Follow-ups (not yet built)

- Opus/Sonnet weekly breakdown + `extra_usage` display.
- API usage providers.
- Code-sign the bundle (unsigned binaries may re-prompt for Keychain access
  after each rebuild).
