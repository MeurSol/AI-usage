//! Watch Claude Code's conversation logs so usage refreshes right after a turn.
//!
//! Claude Code appends to `~/.claude/projects/**/*.jsonl` as a conversation
//! progresses; a completed turn writes the assistant message (by which point
//! the server-side usage is already updated). We signal `tx` on those writes
//! and let the poller debounce + fetch.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Start watching the projects dir. Returns the watcher, which the caller must
/// keep alive. `None` (e.g. dir missing) means the poller falls back to its
/// periodic interval only.
pub fn spawn(tx: Sender<()>) -> Option<RecommendedWatcher> {
    let dir = projects_dir()?;
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            let touches_jsonl = event
                .paths
                .iter()
                .any(|p| p.extension().is_some_and(|e| e == "jsonl"));
            if touches_jsonl && matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                let _ = tx.send(());
            }
        }
    })
    .ok()?;
    watcher.watch(&dir, RecursiveMode::Recursive).ok()?;
    Some(watcher)
}

fn projects_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let dir = Path::new(&home).join(".claude/projects");
    dir.is_dir().then_some(dir)
}
