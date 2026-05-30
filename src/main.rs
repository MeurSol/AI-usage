mod keychain;
mod menubar;
mod poller;
mod provider;

use std::time::Duration;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

use provider::claude::ClaudeProvider;

const POLL_INTERVAL: Duration = Duration::from_secs(60);

fn main() {
    let shared = poller::spawn(ClaudeProvider::new(), POLL_INTERVAL);

    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // Keep the controller alive for the lifetime of the app.
    let _controller = menubar::install(mtm, shared);

    app.run();
}
