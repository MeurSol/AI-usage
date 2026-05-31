//! macOS status bar item + dropdown menu, driven by the shared `AppState`.
//! A 1-second NSTimer ticks the controller, which re-reads the state and
//! redraws the title and menu. All AppKit access stays on the main thread.

use chrono::{DateTime, Local};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSCellImagePosition, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{NSString, NSTimer};

use crate::gauge;
use crate::poller::{Shared, Status, Trigger};

pub struct Ivars {
    shared: Shared,
    trigger: Trigger,
    item: Retained<NSStatusItem>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AIUsageController"]
    #[ivars = Ivars]
    pub(crate) struct Controller;

    impl Controller {
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: Option<&NSTimer>) {
            self.refresh();
        }
    }

    unsafe impl NSObjectProtocol for Controller {}

    // Fetch fresh numbers right when the user opens the dropdown.
    unsafe impl NSMenuDelegate for Controller {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, _menu: &NSMenu) {
            self.ivars().trigger.fire();
        }
    }
);

impl Controller {
    fn refresh(&self) {
        let mtm = MainThreadMarker::from(self);
        let (session_frac, title, lines) = render(&self.ivars().shared);

        if let Some(button) = self.ivars().item.button(mtm) {
            match session_frac {
                Some(frac) => {
                    button.setImage(Some(&gauge::session_gauge(frac / 100.0)));
                    button.setImagePosition(NSCellImagePosition::ImageLeft);
                }
                None => button.setImage(None),
            }
            button.setTitle(&NSString::from_str(&title));
        }

        let menu = NSMenu::new(mtm);
        for line in &lines {
            let mi = NSMenuItem::new(mtm);
            mi.setTitle(&NSString::from_str(line));
            menu.addItem(&mi);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));

        // nil target => `terminate:` is resolved up the responder chain to NSApp.
        let quit = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str("Quit"),
                Some(sel!(terminate:)),
                &NSString::from_str("q"),
            )
        };
        menu.addItem(&quit);

        // Delegate fires menuNeedsUpdate: → a fresh fetch when the menu opens.
        menu.setDelegate(Some(ProtocolObject::from_ref(self)));
        self.ivars().item.setMenu(Some(&menu));
    }
}

/// Build the status item, controller and refresh timer. The returned
/// `Controller` (and the status item it holds) must be kept alive.
pub fn install(mtm: MainThreadMarker, shared: Shared, trigger: Trigger) -> Retained<Controller> {
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = item.button(mtm) {
        button.setTitle(&NSString::from_str("…"));
    }

    let controller = {
        let this = mtm
            .alloc::<Controller>()
            .set_ivars(Ivars { shared, trigger, item });
        let this: Retained<Controller> = unsafe { msg_send![super(this), init] };
        this
    };

    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
            1.0,
            &controller,
            sel!(tick:),
            None,
            true,
        );
    }
    controller.refresh();
    controller
}

/// Produce the session gauge fraction (session %, `None` when no data), the bar
/// title, and the dropdown lines for the current state.
fn render(shared: &Shared) -> (Option<f64>, String, Vec<String>) {
    let state = shared.lock().expect("state lock");
    match &state.snapshot {
        Some(snap) => {
            let pcts: Vec<String> = snap
                .windows
                .iter()
                .map(|w| format!("{:.0}%", w.utilization))
                .collect();
            let title = match state.status {
                Status::Stale => format!("{} ·", pcts.join(" / ")),
                _ => pcts.join(" / "),
            };

            let mut lines: Vec<String> = snap
                .windows
                .iter()
                .map(|w| {
                    format!(
                        "{}   {:.0}%   resets {}",
                        w.label,
                        w.utilization,
                        fmt_reset(w.resets_at)
                    )
                })
                .collect();
            if state.status == Status::Stale {
                lines.push("⚠︎ offline — showing last update".into());
            }
            let session = snap.windows.first().map(|w| w.utilization);
            (session, title, lines)
        }
        None => match state.status {
            Status::AuthExpired => (
                None,
                "auth?".into(),
                vec!["Auth expired — re-login in Claude Code".into()],
            ),
            Status::Error => (
                None,
                "—".into(),
                vec![format!(
                    "Error: {}",
                    state.message.clone().unwrap_or_default()
                )],
            ),
            _ => (None, "…".into(), vec!["Loading…".into()]),
        },
    }
}

/// Time-of-day if the reset is today, otherwise an abbreviated date + time.
fn fmt_reset(dt: DateTime<Local>) -> String {
    if dt.date_naive() == Local::now().date_naive() {
        dt.format("%H:%M").to_string()
    } else {
        dt.format("%b %-d, %H:%M").to_string()
    }
}
