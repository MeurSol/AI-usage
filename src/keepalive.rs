//! Nudge Claude Code into renewing the Keychain credential.
//!
//! The stored access token lives about eight hours and only Claude Code
//! renews it — on its next API call, writing the result back to the Keychain.
//! After an idle stretch nothing has renewed it and there is nothing fresh for
//! us to read.
//!
//! We deliberately do not run the OAuth refresh ourselves. The refresh_token
//! rotates on use, so a third party refreshing it invalidates the copy any
//! running Claude Code session holds. Instead we run Claude Code itself,
//! exactly as opening a second terminal would, and leave it the sole owner of
//! the credential lifecycle.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

/// Floor on the gap between attempts. A token that is stale because the grant
/// itself died can only be fixed by `/login`, so retrying every poll would
/// spawn a process a minute for nothing.
const MIN_GAP: Duration = Duration::from_secs(30 * 60);

/// Abandon a run that hangs — a wedged network, or a prompt we cannot answer.
const TIMEOUT: Duration = Duration::from_secs(90);

static LAST_ATTEMPT: Mutex<Option<Instant>> = Mutex::new(None);

/// Run Claude Code once so it renews the credential. Returns whether it
/// completed successfully; the caller re-reads the Keychain either way.
///
/// Rate-limited to one attempt per `MIN_GAP`, so a caller in a retry loop can
/// call this unconditionally.
pub fn nudge() -> bool {
    if !claim_attempt() {
        return false;
    }
    match claude_binary() {
        Some(bin) => run(&bin),
        None => false,
    }
}

/// Reserve the next attempt slot, or report that one was used too recently.
fn claim_attempt() -> bool {
    let Ok(mut last) = LAST_ATTEMPT.lock() else {
        return false;
    };
    let now = Instant::now();
    if last.is_some_and(|t| now.duration_since(t) < MIN_GAP) {
        return false;
    }
    *last = Some(now);
    true
}

/// Locate the `claude` executable. launchd gives us a bare PATH that excludes
/// every usual install prefix, so the well-known locations come first and the
/// inherited PATH is only a fallback.
fn claude_binary() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let well_known = [
        PathBuf::from("/opt/homebrew/bin/claude"),
        PathBuf::from("/usr/local/bin/claude"),
    ];
    let under_home = home
        .into_iter()
        .flat_map(|h| [h.join(".local/bin/claude"), h.join(".claude/local/claude")]);
    let on_path: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.join("claude")).collect())
        .unwrap_or_default();

    well_known
        .into_iter()
        .chain(under_home)
        .chain(on_path)
        .find(|p| is_executable(p))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The cheapest invocation that still makes Claude Code authenticate: one
/// print-mode turn on the smallest model, in a scratch directory, with no
/// session written to disk. `--bare` would be cheaper still but skips the
/// credential sources, leaving it unauthenticated.
fn run(bin: &Path) -> bool {
    let Ok(mut child) = Command::new(bin)
        .args([
            "--no-session-persistence",
            "--model",
            "haiku",
            "-p",
            "ok",
        ])
        // A long-lived token in the environment bypasses the Keychain
        // entirely, which would defeat the whole point of this call.
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };

    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => thread::sleep(Duration::from_millis(200)),
            Err(_) => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    /// Spawns the real Claude Code and costs a (tiny) API call, so it is not
    /// part of the default run. Use it after changing the invocation or the
    /// binary lookup: `cargo test -- --ignored keepalive`.
    #[test]
    #[ignore = "spawns Claude Code and makes a real API call"]
    fn nudge_runs_claude_code() {
        assert!(super::claude_binary().is_some(), "claude not found");
        assert!(super::nudge(), "claude ran but did not exit successfully");
    }
}
