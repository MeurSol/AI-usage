mod keychain;
mod provider;

use provider::claude::ClaudeProvider;
use provider::Provider;

// Temporary CLI entry point used to verify the data path before the menu bar
// UI lands. Replaced in the UI step.
fn main() {
    let p = ClaudeProvider::new();
    match p.fetch() {
        Ok(snap) => {
            for w in &snap.windows {
                println!(
                    "{:<14} {:>5.0}%  resets {}",
                    w.label,
                    w.utilization,
                    w.resets_at.format("%Y-%m-%d %H:%M")
                );
            }
        }
        Err(e) => eprintln!("fetch failed: {e:?}"),
    }
}
