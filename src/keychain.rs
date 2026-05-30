//! Read the Claude Code OAuth access token from the macOS Keychain.
//!
//! Claude Code stores credentials as a generic-password item under the service
//! `Claude Code-credentials`, keyed by the macOS account (login short name).
//! The first run of this app will trigger a one-time Keychain access prompt
//! ("Always Allow" to silence future reads).

use anyhow::{Context, Result};
use serde::Deserialize;

const SERVICE: &str = "Claude Code-credentials";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Creds {
    claude_ai_oauth: OauthCreds,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OauthCreds {
    access_token: String,
}

/// Return the current Claude Code OAuth access token, freshly read each call
/// (Claude Code refreshes it in place while in use).
pub fn claude_access_token() -> Result<String> {
    let account = account_name().context("cannot determine macOS account name")?;
    let blob = security_framework::passwords::get_generic_password(SERVICE, &account)
        .context("read 'Claude Code-credentials' from Keychain")?;
    let creds: Creds =
        serde_json::from_slice(&blob).context("parse keychain credential JSON")?;
    Ok(creds.claude_ai_oauth.access_token)
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
