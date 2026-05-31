mod gauge;
mod keychain;
mod menubar;
mod poller;
mod provider;
mod token;
mod watch;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

use provider::claude::ClaudeProvider;

fn main() {
    let (shared, trigger) = poller::spawn(ClaudeProvider::new());

    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // Keep the controller alive for the lifetime of the app.
    let _controller = menubar::install(mtm, shared, trigger);

    app.run();
}
