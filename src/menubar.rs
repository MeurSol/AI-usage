//! macOS status bar item + dropdown menu, driven by the shared `AppState`.
//! A short-interval NSTimer ticks the controller: it animates a spinner while a
//! fetch is in flight, and otherwise redraws the title and menu only when the
//! state's `version` changed, so idle ticks stay cheap. All AppKit access stays
//! on the main thread.

use std::cell::Cell;

use chrono::{DateTime, Local};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSCellImagePosition, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSString, NSTimer};

use crate::gauge;
use crate::poller::{AppState, Shared, Status, Trigger};

pub struct Ivars {
    shared: Shared,
    trigger: Trigger,
    item: Retained<NSStatusItem>,
    /// Last `AppState.version` rendered; skip redraw when unchanged.
    last_version: Cell<Option<u64>>,
    /// Spinner rotation in radians, advanced each tick while a fetch is in flight.
    spin_phase: Cell<f64>,
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

        // "Refresh now" menu item: the only user-driven query. Reflect the click
        // immediately — stamp the action time and raise the spinner — so the
        // "Last refresh" line and gauge move the instant you click, regardless of
        // whether the fetch is debounced, rate-limited, or fails. Then fire the
        // trigger so the poller runs a real fetch; apply() overwrites these with
        // the real outcome when it lands.
        #[unsafe(method(refreshNow:))]
        fn refresh_now(&self, _sender: Option<&NSMenuItem>) {
            {
                let mut state = self.ivars().shared.lock().expect("state lock");
                state.checked_at = Some(Local::now());
                state.refreshing = true;
                state.version = state.version.wrapping_add(1);
            }
            self.ivars().trigger.fire();
        }
    }

    unsafe impl NSObjectProtocol for Controller {}
);

impl Controller {
    fn refresh(&self) {
        let mtm = MainThreadMarker::from(self);
        // Snapshot under one lock: whether a fetch is in flight, and (only when
        // the state version changed) a fresh render. The gauge slot spins while
        // fetching; the title + dropdown rebuild whenever the version changed —
        // including the version bump from a Refresh-now click — so the click's
        // result shows even while the spinner is still turning.
        let (refreshing, redraw) = {
            let state = self.ivars().shared.lock().expect("state lock");
            let redraw = if self.ivars().last_version.get() == Some(state.version) {
                None
            } else {
                self.ivars().last_version.set(Some(state.version));
                Some(render(&state))
            };
            (state.refreshing, redraw)
        };

        // Idle tick: nothing in flight and nothing new to draw — bail cheaply.
        if !refreshing && redraw.is_none() {
            return;
        }

        // Gauge slot: spinner while fetching, otherwise the rendered fraction.
        if let Some(button) = self.ivars().item.button(mtm) {
            if refreshing {
                let phase = self.ivars().spin_phase.get() + 0.45;
                self.ivars().spin_phase.set(phase);
                button.setImage(Some(&gauge::spinner(phase)));
                button.setImagePosition(NSCellImagePosition::ImageLeft);
            } else if let Some((session_frac, _, _)) = &redraw {
                match session_frac {
                    Some(frac) => {
                        button.setImage(Some(&gauge::session_gauge(frac / 100.0)));
                        button.setImagePosition(NSCellImagePosition::ImageLeft);
                    }
                    None => button.setImage(None),
                }
            }
        }

        // Title + dropdown: only when the version changed.
        let Some((_, title, lines)) = redraw else {
            return;
        };
        if let Some(button) = self.ivars().item.button(mtm) {
            button.setTitle(&NSString::from_str(&title));
        }

        let menu = NSMenu::new(mtm);
        for line in &lines {
            let mi = NSMenuItem::new(mtm);
            mi.setTitle(&NSString::from_str(line));
            menu.addItem(&mi);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));

        // Manual query. Target is the controller itself (the delegate), which
        // implements refreshNow:.
        let refresh = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str("Refresh now"),
                Some(sel!(refreshNow:)),
                &NSString::from_str("r"),
            )
        };
        unsafe { refresh.setTarget(Some(&**self)) };
        menu.addItem(&refresh);

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
        let this = mtm.alloc::<Controller>().set_ivars(Ivars {
            shared,
            trigger,
            item,
            last_version: Cell::new(None),
            spin_phase: Cell::new(0.0),
        });
        let this: Retained<Controller> = unsafe { msg_send![super(this), init] };
        this
    };

    // ~10 fps so the spinner is smooth while a fetch is in flight; idle ticks
    // are version-gated and return early, so they stay cheap.
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
            0.1,
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
/// title, and the dropdown lines for the current state. The dropdown always ends
/// with a "Last refresh" line so the result of every fetch is visible.
fn render(state: &AppState) -> (Option<f64>, String, Vec<String>) {
    let (session, title, mut lines) = match &state.snapshot {
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

            let lines: Vec<String> = snap
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
            let session = snap.windows.first().map(|w| w.utilization);
            (session, title, lines)
        }
        None => match state.status {
            Status::AuthExpired => (
                None,
                "auth?".into(),
                vec!["Auth expired — re-login in Claude Code".into()],
            ),
            Status::Error => (None, "—".into(), Vec::new()),
            _ => (None, "…".into(), vec!["Loading…".into()]),
        },
    };
    lines.push(last_refresh_line(state));
    (session, title, lines)
}

/// The "Last refresh" dropdown line: when the last fetch attempt completed and
/// how it went, so a Refresh-now click always shows a concrete result.
fn last_refresh_line(state: &AppState) -> String {
    let when = state
        .checked_at
        .map_or_else(|| "—".into(), |t| t.format("%H:%M:%S").to_string());
    let outcome = if state.refreshing {
        "checking…".into()
    } else {
        match state.status {
            Status::Loading => "checking…".into(),
            Status::Ok => "OK".into(),
            Status::AuthExpired => "auth expired — re-login in Claude Code".into(),
            // Always surface the reason a refresh failed; "(kept last)" notes the
            // bar is still showing the previous good snapshot.
            Status::Stale | Status::Error => {
                let msg = state.message.as_deref().unwrap_or("unknown error");
                let reason = if msg.contains("429") {
                    "rate-limited (429)"
                } else {
                    msg
                };
                let kept = if state.status == Status::Stale {
                    " (kept last)"
                } else {
                    ""
                };
                format!("failed — {reason}{kept}")
            }
        }
    };
    format!("Last refresh {when} · {outcome}")
}

/// Time-of-day if the reset is today, otherwise an abbreviated date + time.
fn fmt_reset(dt: DateTime<Local>) -> String {
    if dt.date_naive() == Local::now().date_naive() {
        dt.format("%H:%M").to_string()
    } else {
        dt.format("%b %-d, %H:%M").to_string()
    }
}
