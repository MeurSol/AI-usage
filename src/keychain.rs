//! Read/write the Claude Code OAuth credentials in the macOS Keychain.
//!
//! Stored as a generic-password item under service `Claude Code-credentials`,
//! account = login short name, value = a JSON blob. The first access may
//! trigger a one-time Keychain prompt ("Always Allow").

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

const SERVICE: &str = "Claude Code-credentials";

/// The fields we care about, plus the full blob so we can write it back
/// without dropping any keys we don't model.
pub struct Credentials {
    pub access_token: String,
    pub refresh_token: String,
    /// Expiry in epoch milliseconds.
    pub expires_at_ms: i64,
    raw: Value,
}

pub fn load() -> Result<Credentials> {
    let account = account_name().context("cannot determine macOS account name")?;
    let blob = security_framework::passwords::get_generic_password(SERVICE, &account)
        .context("read 'Claude Code-credentials' from Keychain")?;
    let raw: Value = serde_json::from_slice(&blob).context("parse keychain credential JSON")?;
    let oauth = raw
        .get("claudeAiOauth")
        .ok_or_else(|| anyhow!("missing claudeAiOauth in keychain blob"))?;
    Ok(Credentials {
        access_token: str_field(oauth, "accessToken")?,
        refresh_token: str_field(oauth, "refreshToken")?,
        expires_at_ms: oauth
            .get("expiresAt")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("missing/invalid expiresAt"))?,
        raw,
    })
}

/// Persist refreshed tokens back to the Keychain, preserving every other field
/// so Claude Code keeps working with the rotated credentials.
pub fn store_refreshed(
    base: &Credentials,
    access_token: &str,
    refresh_token: &str,
    expires_at_ms: i64,
) -> Result<()> {
    let account = account_name().context("cannot determine macOS account name")?;
    let mut blob = base.raw.clone();
    let oauth = blob
        .get_mut("claudeAiOauth")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow!("claudeAiOauth not an object"))?;
    oauth.insert("accessToken".into(), Value::String(access_token.into()));
    oauth.insert("refreshToken".into(), Value::String(refresh_token.into()));
    oauth.insert("expiresAt".into(), Value::from(expires_at_ms));
    let bytes = serde_json::to_vec(&blob).context("serialize credential JSON")?;
    security_framework::passwords::set_generic_password(SERVICE, &account, &bytes)
        .context("write refreshed credentials to Keychain")
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
