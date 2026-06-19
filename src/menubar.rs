//! macOS status bar item + dropdown menu, driven by the shared `AppState`.
//!
//! The dropdown is built once and its rows are updated *in place* every tick, so
//! an already-open menu reflects new fetch results live (the timer runs in the
//! common run-loop modes so it keeps firing while the menu is tracking, and the
//! "Refresh now" row is a button view so clicking it doesn't dismiss the menu).
//! Rows use attributed titles — colored percentages, secondary-gray reset times
//! — for a tidier look. A short-interval NSTimer animates the in-flight spinner
//! and otherwise redraws only when the state's `version` changed. All AppKit
//! access stays on the main thread.

use std::cell::Cell;

use chrono::{DateTime, Local};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{
    define_class, msg_send, sel, AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSButton, NSCellImagePosition, NSColor, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSTextAlignment,
    NSVariableStatusItemLength,
};
use objc2_foundation::{
    NSAttributedString, NSMutableAttributedString, NSPoint, NSRange, NSRect, NSRunLoop,
    NSRunLoopCommonModes, NSSize, NSString, NSTimer,
};

use crate::gauge;
use crate::poller::{AppState, Shared, Status, Trigger};

pub struct Ivars {
    shared: Shared,
    trigger: Trigger,
    item: Retained<NSStatusItem>,
    /// Persistent dropdown rows, updated in place so an open menu stays live.
    session_item: Retained<NSMenuItem>,
    weekly_item: Retained<NSMenuItem>,
    last_refresh_item: Retained<NSMenuItem>,
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

        // "Refresh now" button row: the only user-driven query. Reflect the click
        // immediately — stamp the action time and raise the spinner — so the row
        // and gauge move at once, even if the fetch is debounced, rate-limited, or
        // fails. The row is a button view so the menu stays open and the live
        // in-place update is visible; apply() overwrites with the real outcome.
        #[unsafe(method(refreshNow:))]
        fn refresh_now(&self, _sender: Option<&NSObject>) {
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
        // the version changed) a fresh render. The gauge slot spins while
        // fetching; rows rebuild whenever the version changed — including the bump
        // from a Refresh-now click — so the click's result shows while spinning.
        let (refreshing, view) = {
            let state = self.ivars().shared.lock().expect("state lock");
            let view = if self.ivars().last_version.get() == Some(state.version) {
                None
            } else {
                self.ivars().last_version.set(Some(state.version));
                Some(render(&state))
            };
            (state.refreshing, view)
        };

        // Idle tick: nothing in flight and nothing new to draw — bail cheaply.
        if !refreshing && view.is_none() {
            return;
        }

        // Status-bar button: spinner while fetching, else the rendered gauge.
        if let Some(button) = self.ivars().item.button(mtm) {
            if refreshing {
                let phase = self.ivars().spin_phase.get() + 0.45;
                self.ivars().spin_phase.set(phase);
                button.setImage(Some(&gauge::spinner(phase)));
                button.setImagePosition(NSCellImagePosition::ImageLeft);
            } else if let Some(v) = &view {
                match v.session_frac {
                    Some(frac) => {
                        button.setImage(Some(&gauge::session_gauge(frac / 100.0)));
                        button.setImagePosition(NSCellImagePosition::ImageLeft);
                    }
                    None => button.setImage(None),
                }
            }
            if let Some(v) = &view {
                button.setTitle(&NSString::from_str(&v.bar_title));
            }
        }

        // Dropdown rows: only when the version changed.
        if let Some(v) = view {
            self.apply_view(&v);
        }
    }

    /// Update the persistent dropdown rows in place from a fresh render.
    fn apply_view(&self, view: &View) {
        let label = NSColor::labelColor();
        let secondary = NSColor::secondaryLabelColor();
        let body = NSFont::menuFontOfSize(13.0);
        let bold = NSFont::boldSystemFontOfSize(13.0);
        let small = NSFont::systemFontOfSize(11.0);

        let set_usage = |item: &NSMenuItem, row: &Row| match row {
            Row::Usage { label: name, pct, reset } => {
                let title = attributed(vec![
                    (format!("{name}    "), label.clone(), body.clone()),
                    (format!("{pct:.0}%"), gauge::level_color(pct / 100.0), bold.clone()),
                    (format!("    resets {reset}"), secondary.clone(), small.clone()),
                ]);
                item.setAttributedTitle(Some(&title));
            }
            Row::Note(text) => {
                let title = attributed(vec![(text.clone(), secondary.clone(), body.clone())]);
                item.setAttributedTitle(Some(&title));
            }
        };

        set_usage(&self.ivars().session_item, &view.rows[0]);
        match view.rows.get(1) {
            Some(row) => {
                self.ivars().weekly_item.setHidden(false);
                set_usage(&self.ivars().weekly_item, row);
            }
            None => self.ivars().weekly_item.setHidden(true),
        }

        let last = attributed(vec![(view.last_refresh.clone(), secondary, small)]);
        self.ivars().last_refresh_item.setAttributedTitle(Some(&last));
    }
}

/// Build the status item, controller, dropdown and refresh timer. The returned
/// `Controller` (and the status item it holds) must be kept alive.
pub fn install(mtm: MainThreadMarker, shared: Shared, trigger: Trigger) -> Retained<Controller> {
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = item.button(mtm) {
        button.setTitle(&NSString::from_str("…"));
    }

    let session_item = NSMenuItem::new(mtm);
    let weekly_item = NSMenuItem::new(mtm);
    let last_refresh_item = NSMenuItem::new(mtm);

    let controller = {
        let this = mtm.alloc::<Controller>().set_ivars(Ivars {
            shared,
            trigger,
            item: item.clone(),
            session_item: session_item.clone(),
            weekly_item: weekly_item.clone(),
            last_refresh_item: last_refresh_item.clone(),
            last_version: Cell::new(None),
            spin_phase: Cell::new(0.0),
        });
        let this: Retained<Controller> = unsafe { msg_send![super(this), init] };
        this
    };

    let menu = NSMenu::new(mtm);
    // We manage enablement ourselves so info rows keep their attributed colors
    // (auto-enable greys out actionless items).
    menu.setAutoenablesItems(false);

    let header = NSMenuItem::new(mtm);
    header.setAttributedTitle(Some(&attributed(vec![(
        "Claude usage".into(),
        NSColor::secondaryLabelColor(),
        NSFont::systemFontOfSize(11.0),
    )])));
    header.setEnabled(false);
    menu.addItem(&header);

    session_item.setEnabled(true); // keep the colored percentage
    weekly_item.setEnabled(true);
    menu.addItem(&session_item);
    menu.addItem(&weekly_item);

    menu.addItem(&NSMenuItem::separatorItem(mtm));
    last_refresh_item.setEnabled(false);
    menu.addItem(&last_refresh_item);
    menu.addItem(&NSMenuItem::separatorItem(mtm));

    // Refresh as a button view so clicking it keeps the menu open (a plain item
    // would dismiss it before the live result could show).
    let refresh_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("\u{21bb}   Refresh now"),
            Some(&controller),
            Some(sel!(refreshNow:)),
            mtm,
        )
    };
    refresh_button.setBordered(false);
    refresh_button.setAlignment(NSTextAlignment::Left);
    refresh_button.setFont(Some(&NSFont::menuFontOfSize(13.0)));
    refresh_button.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(220.0, 22.0),
    ));
    let refresh_item = NSMenuItem::new(mtm);
    refresh_item.setView(Some(&refresh_button));
    menu.addItem(&refresh_item);

    // nil target => `terminate:` is resolved up the responder chain to NSApp.
    let quit = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Quit"),
            Some(sel!(terminate:)),
            &NSString::from_str("q"),
        )
    };
    quit.setEnabled(true);
    menu.addItem(&quit);

    item.setMenu(Some(&menu));

    // ~10 fps so the spinner is smooth; added in the common modes so it keeps
    // firing (and the menu keeps updating) while the dropdown is open. Idle ticks
    // are version-gated and return early, so they stay cheap.
    let timer = unsafe {
        NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
            0.1,
            &controller,
            sel!(tick:),
            None,
            true,
        )
    };
    unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };

    controller.refresh();
    controller
}

/// Plain (AppKit-free) description of what to draw, computed under the state lock.
struct View {
    /// Session % for the gauge (`None` when there's no data).
    session_frac: Option<f64>,
    /// Menu-bar title text, e.g. `27% / 18%`.
    bar_title: String,
    /// One or two usage rows, or a single note row when there's no data.
    rows: Vec<Row>,
    /// The "Last refresh …" status line.
    last_refresh: String,
}

enum Row {
    Usage { label: String, pct: f64, reset: String },
    Note(String),
}

/// Produce the `View` for the current state.
fn render(state: &AppState) -> View {
    let (session_frac, bar_title, rows) = match &state.snapshot {
        Some(snap) => {
            let pcts: Vec<String> = snap
                .windows
                .iter()
                .map(|w| format!("{:.0}%", w.utilization))
                .collect();
            let bar_title = match state.status {
                Status::Stale => format!("{} ·", pcts.join(" / ")),
                _ => pcts.join(" / "),
            };
            let rows = snap
                .windows
                .iter()
                .map(|w| Row::Usage {
                    label: w.label.clone(),
                    pct: w.utilization,
                    reset: fmt_reset(w.resets_at),
                })
                .collect();
            let session = snap.windows.first().map(|w| w.utilization);
            (session, bar_title, rows)
        }
        None => match state.status {
            Status::AuthExpired => (
                None,
                "auth?".into(),
                vec![Row::Note("Auth expired — re-login in Claude Code".into())],
            ),
            Status::Error => (None, "—".into(), vec![Row::Note("No data yet".into())]),
            _ => (None, "…".into(), vec![Row::Note("Loading…".into())]),
        },
    };
    View {
        session_frac,
        bar_title,
        rows,
        last_refresh: last_refresh_line(state),
    }
}

/// The "Last refresh" line: when the last fetch attempt completed and how it
/// went, so a Refresh-now click always shows a concrete result — and a failure
/// always shows its reason.
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

/// Build a single attributed string from colored/fonted runs. Main thread only.
fn attributed(runs: Vec<(String, Retained<NSColor>, Retained<NSFont>)>) -> Retained<NSAttributedString> {
    let out = NSMutableAttributedString::new();
    for (text, color, font) in runs {
        let piece = NSMutableAttributedString::initWithString(
            NSMutableAttributedString::alloc(),
            &NSString::from_str(&text),
        );
        let range = NSRange::new(0, piece.length());
        unsafe {
            piece.addAttribute_value_range(NSForegroundColorAttributeName, &*color, range);
            piece.addAttribute_value_range(NSFontAttributeName, &*font, range);
        }
        out.appendAttributedString(&piece);
    }
    Retained::into_super(out)
}

/// Time-of-day if the reset is today, otherwise an abbreviated date + time.
fn fmt_reset(dt: DateTime<Local>) -> String {
    if dt.date_naive() == Local::now().date_naive() {
        dt.format("%H:%M").to_string()
    } else {
        dt.format("%b %-d, %H:%M").to_string()
    }
}
