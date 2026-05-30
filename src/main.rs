mod keychain;
mod poller;
mod provider;

use std::time::Duration;

use provider::claude::ClaudeProvider;

// Temporary harness: verify the poller refreshes over two rounds. Replaced by
// the menu bar UI next.
fn main() {
    let shared = poller::spawn(ClaudeProvider::new(), Duration::from_secs(5));
    for round in 0..3 {
        std::thread::sleep(Duration::from_millis(if round == 0 { 1500 } else { 5000 }));
        let s = shared.lock().unwrap();
        let status = match s.status {
            poller::Status::Loading => "loading",
            poller::Status::Ok => "ok",
            poller::Status::Stale => "stale",
            poller::Status::AuthExpired => "auth-expired",
            poller::Status::Error => "error",
        };
        match &s.snapshot {
            Some(snap) => {
                let pcts: Vec<String> =
                    snap.windows.iter().map(|w| format!("{:.0}%", w.utilization)).collect();
                println!("round {round}: status={status} {}", pcts.join(" / "));
            }
            None => println!(
                "round {round}: status={status} (no data) msg={:?}",
                s.message
            ),
        }
    }
}
