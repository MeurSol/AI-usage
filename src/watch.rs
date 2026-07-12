//! Route conversation-log changes to the matching provider worker.
//!
//! Claude writes only wake Claude's network-backed provider. Codex writes only
//! wake GPT's local provider, so a busy Codex session can never cause Claude
//! usage endpoint traffic (and vice versa).

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::poller::Wake;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Claude,
    Gpt,
}

/// Start watching the available usage-log dirs. `None` means only manual,
/// reset, recovery, and sparse safety-recheck triggers remain.
pub fn spawn(
    claude_tx: Option<Sender<Wake>>,
    gpt_tx: Option<Sender<Wake>>,
) -> Option<RecommendedWatcher> {
    let dirs = usage_dirs();
    if dirs.is_empty() {
        return None;
    }
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
        let Ok(event) = result else {
            return;
        };
        if !matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
            return;
        }

        let mut wake_claude = false;
        let mut wake_gpt = false;
        for path in event
            .paths
            .iter()
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        {
            match source_for_path(path) {
                Some(Source::Claude) => wake_claude = true,
                Some(Source::Gpt) => wake_gpt = true,
                None => {}
            }
        }
        if wake_claude {
            if let Some(sender) = &claude_tx {
                let _ = sender.send(Wake::Turn);
            }
        }
        if wake_gpt {
            if let Some(sender) = &gpt_tx {
                let _ = sender.send(Wake::Turn);
            }
        }
    })
    .ok()?;

    let mut watching = false;
    for dir in dirs {
        if watcher.watch(&dir, RecursiveMode::Recursive).is_ok() {
            watching = true;
        }
    }
    watching.then_some(watcher)
}

fn source_for_path(path: &Path) -> Option<Source> {
    for component in path.components() {
        if component.as_os_str() == ".claude" {
            return Some(Source::Claude);
        }
        if component.as_os_str() == ".codex" {
            return Some(Source::Gpt);
        }
    }
    None
}

fn usage_dirs() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    [".claude/projects", ".codex/sessions"]
        .into_iter()
        .map(|relative| Path::new(&home).join(relative))
        .filter(|dir| dir.is_dir())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_claude_and_codex_logs_independently() {
        assert!(matches!(
            source_for_path(Path::new("/Users/me/.claude/projects/p/chat.jsonl")),
            Some(Source::Claude)
        ));
        assert!(matches!(
            source_for_path(Path::new("/Users/me/.codex/sessions/2026/chat.jsonl")),
            Some(Source::Gpt)
        ));
        assert!(source_for_path(Path::new("/tmp/chat.jsonl")).is_none());
    }
}
