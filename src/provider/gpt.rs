//! GPT/Codex provider: reads the latest rate-limit snapshot that Codex writes
//! to its local session JSONL files. This uses the signed-in Codex client's
//! own five-hour and weekly counters, so no API key or token access is needed.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset, Local};
use serde::Deserialize;

use super::{FetchError, Provider, UsageSnapshot, UsageWindow};

/// A token-count event is near the end of an active log. Limit I/O so old,
/// long conversations do not make a menu refresh expensive.
const TAIL_BYTES: u64 = 512 * 1024;
/// Concurrent Codex sessions can update different files. Checking the newest
/// handful lets us select the newest server snapshot across those sessions.
const MAX_RECENT_LOGS: usize = 32;

#[derive(Clone, Deserialize)]
struct RawLimits {
    primary: RawWindow,
    #[serde(default)]
    secondary: Option<RawWindow>,
}

#[derive(Clone, Deserialize)]
struct RawWindow {
    used_percent: f64,
    window_minutes: i64,
    resets_at: i64,
}

#[derive(Deserialize)]
struct Event {
    timestamp: DateTime<FixedOffset>,
    #[serde(rename = "type")]
    kind: String,
    payload: Payload,
}

#[derive(Deserialize)]
struct Payload {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    rate_limits: Option<RawLimits>,
}

pub struct GptProvider {
    sessions_dir: PathBuf,
}

impl GptProvider {
    pub fn new() -> Self {
        let sessions_dir = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("~"))
            .join(".codex/sessions");
        Self { sessions_dir }
    }

    #[cfg(test)]
    fn at(sessions_dir: PathBuf) -> Self {
        Self { sessions_dir }
    }

    fn read_latest(&self) -> Result<UsageSnapshot> {
        if !self.sessions_dir.is_dir() {
            return Ok(empty_snapshot());
        }
        let mut logs = Vec::new();
        collect_logs(&self.sessions_dir, &mut logs)
            .with_context(|| format!("scan {}", self.sessions_dir.display()))?;
        logs.sort_by_key(|path| {
            std::cmp::Reverse(
                fs::metadata(path)
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH),
            )
        });

        let mut latest: Option<(DateTime<FixedOffset>, RawLimits)> = None;
        for path in logs.iter().take(MAX_RECENT_LOGS) {
            let text = read_tail(path).with_context(|| format!("read {}", path.display()))?;
            // Events are append-only, so the first matching line from the end is
            // the newest rate-limit snapshot in this file.
            if let Some(candidate) = text.lines().rev().find_map(parse_rate_limits) {
                if latest.as_ref().is_none_or(|(time, _)| candidate.0 > *time) {
                    latest = Some(candidate);
                }
            }
        }

        Ok(latest
            .map(|(_, limits)| snapshot(limits))
            .unwrap_or_else(empty_snapshot))
    }
}

impl Provider for GptProvider {
    fn name(&self) -> &'static str {
        "GPT"
    }

    fn fetch(&self) -> Result<UsageSnapshot, FetchError> {
        self.read_latest().map_err(FetchError::Other)
    }
}

fn collect_logs(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ty = entry.file_type()?;
        if ty.is_dir() {
            collect_logs(&path, out)?;
        } else if ty.is_file() && path.extension().is_some_and(|ext| ext == "jsonl") {
            out.push(path);
        }
    }
    Ok(())
}

fn read_tail(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut bytes)?;
    // If we started in the middle of a JSON line, discard that partial record.
    if start > 0 {
        if let Some(pos) = bytes.iter().position(|&byte| byte == b'\n') {
            bytes.drain(..=pos);
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn parse_rate_limits(line: &str) -> Option<(DateTime<FixedOffset>, RawLimits)> {
    let event: Event = serde_json::from_str(line).ok()?;
    if event.kind != "event_msg" || event.payload.kind != "token_count" {
        return None;
    }
    let limits = event.payload.rate_limits?;
    Some((event.timestamp, limits))
}

fn snapshot(limits: RawLimits) -> UsageSnapshot {
    let mut windows = vec![window("Session (5h)", limits.primary)];
    if let Some(secondary) = limits.secondary {
        windows.push(window("Weekly (7d)", secondary));
    }
    UsageSnapshot { windows }
}

fn empty_snapshot() -> UsageSnapshot {
    UsageSnapshot {
        windows: vec![
            UsageWindow {
                label: "Session (5h)".into(),
                utilization: 0.0,
                resets_at: None,
            },
            UsageWindow {
                label: "Weekly (7d)".into(),
                utilization: 0.0,
                resets_at: None,
            },
        ],
    }
}

fn window(default_label: &str, raw: RawWindow) -> UsageWindow {
    let reset = DateTime::from_timestamp(raw.resets_at, 0).map(|dt| dt.with_timezone(&Local));
    // A local event cannot update itself at rollover. Once its reset has passed,
    // the correct current usage is zero until a later Codex turn writes a new
    // server snapshot.
    let active = reset.is_some_and(|time| time > Local::now());
    let label = match raw.window_minutes {
        300 => "Session (5h)".into(),
        10_080 => "Weekly (7d)".into(),
        _ => default_label.into(),
    };
    UsageWindow {
        label,
        utilization: if active { raw.used_percent } else { 0.0 },
        resets_at: active.then_some(reset).flatten(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENT: &str = r#"{"timestamp":"2026-07-12T06:27:01.566Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{"limit_id":"codex","primary":{"used_percent":8.0,"window_minutes":300,"resets_at":4102444800},"secondary":{"used_percent":12.0,"window_minutes":10080,"resets_at":4103049600}}}}"#;

    #[test]
    fn parses_codex_session_and_weekly_limits() {
        let (_, limits) = parse_rate_limits(EVENT).expect("rate-limit event");
        let snap = snapshot(limits);
        assert_eq!(snap.windows.len(), 2);
        assert_eq!(snap.windows[0].label, "Session (5h)");
        assert_eq!(snap.windows[0].utilization, 8.0);
        assert_eq!(snap.windows[1].label, "Weekly (7d)");
        assert_eq!(snap.windows[1].utilization, 12.0);
    }

    #[test]
    fn ignores_unrelated_events() {
        let line = r#"{"timestamp":"2026-07-12T06:27:01Z","type":"event_msg","payload":{"type":"agent_message","rate_limits":null}}"#;
        assert!(parse_rate_limits(line).is_none());
    }

    #[test]
    fn expired_local_snapshot_rolls_to_zero() {
        let raw = RawWindow {
            used_percent: 99.0,
            window_minutes: 300,
            resets_at: 1,
        };
        let window = window("Session (5h)", raw);
        assert_eq!(window.utilization, 0.0);
        assert!(window.resets_at.is_none());
    }

    #[test]
    fn missing_directory_reports_zero_until_codex_creates_a_session() {
        let provider = GptProvider::at(PathBuf::from("/definitely/not/a/codex-dir"));
        let snap = provider.read_latest().expect("empty snapshot");
        assert_eq!(snap.windows.len(), 2);
        assert_eq!(snap.windows[0].utilization, 0.0);
        assert_eq!(snap.windows[1].utilization, 0.0);
    }
}
