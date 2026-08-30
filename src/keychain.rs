//! Read the Claude Code OAuth credentials from the macOS Keychain.
//!
//! Stored as a generic-password item under service `Claude Code-credentials`,
//! account = login short name, value = a JSON blob.
//!
//! Two deliberate constraints, both about the item's *partition list* — the
//! gate macOS checks above the ACL, and the one "Always Allow" cannot edit:
//!
//! 1. Read-only. Writing the item rewrites its partition list to the writing
//!    process's code identity, evicting Claude Code's `teamid:Q6L2SF6YDW` and
//!    making it prompt for Keychain access on every run.
//! 2. Read through `/usr/bin/security`, not the Security framework in-process.
//!    The Apple-signed tool sits in the stable `apple-tool:` partition; an
//!    in-process read would need a `cdhash:` partition that every rebuild of
//!    this binary invalidates.

use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

const SERVICE: &str = "Claude Code-credentials";

/// The fields we care about from the credential blob.
pub struct Credentials {
    pub access_token: String,
    /// Expiry in epoch milliseconds.
    pub expires_at_ms: i64,
}

pub fn load() -> Result<Credentials> {
    let account = account_name().context("cannot determine macOS account name")?;
    let blob = read_item(&account).context("read 'Claude Code-credentials' from Keychain")?;
    let raw: Value = serde_json::from_str(&blob).context("parse keychain credential JSON")?;
    let oauth = raw
        .get("claudeAiOauth")
        .ok_or_else(|| anyhow!("missing claudeAiOauth in keychain blob"))?;
    Ok(Credentials {
        access_token: str_field(oauth, "accessToken")?,
        expires_at_ms: oauth
            .get("expiresAt")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("missing/invalid expiresAt"))?,
    })
}

/// The item's value, via the Keychain tool. Only stderr is ever quoted back:
/// stdout is the credential itself and must not reach a log line.
fn read_item(account: &str) -> Result<String> {
    let out = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-w", "-s", SERVICE, "-a", account])
        .output()
        .context("spawn /usr/bin/security")?;
    if !out.status.success() {
        bail!(
            "security {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let blob = String::from_utf8(out.stdout).context("keychain blob is not UTF-8")?;
    Ok(blob.trim().to_owned())
}

fn str_field(obj: &Value, key: &str) -> Result<String> {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("missing/invalid {key}"))
}

/// The login short name, used as the Keychain account. Env vars are unset in
/// some launchd contexts, so fall back to the `$HOME` directory name.
fn account_name() -> Option<String> {
    for key in ["USER", "LOGNAME"] {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    let home = std::env::var_os("HOME")?;
    std::path::Path::new(&home)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}
