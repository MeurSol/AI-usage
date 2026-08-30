//! macOS status bar item + dropdown menu, driven by the shared `AppState`.
//!
//! The dropdown is built once and its rows are updated *in place* every tick, so
//! an already-open menu reflects new fetch results live (the timer runs in the
//! common run-loop modes so it keeps firing while the menu is tracking, and the
//! "Refresh now" row is a custom view — `RefreshRow` — that handles its own
//! click so the menu stays open and draws a native hover highlight). Rows use
//! attributed titles at a single size for a tidy, readable look. A
//! short-interval NSTimer animates the in-flight spinner and otherwise redraws
//! only when the state's `version` changed. All AppKit access is on the main
//! thread.

use std::cell::Cell;

use chrono::{DateTime, Local};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{
    define_class, msg_send, sel, AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSAttributedStringNSStringDrawing, NSAutoresizingMaskOptions, NSBezierPath,
    NSCellImagePosition, NSColor, NSEvent, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSTrackingArea,
    NSTrackingAreaOptions, NSVariableStatusItemLength, NSView,
};
use objc2_foundation::{
    NSAttributedString, NSMutableAttributedString, NSPoint, NSRange, NSRect, NSRunLoop,
    NSRunLoopCommonModes, NSSize, NSString, NSTimer,
};

use crate::gauge;
use crate::poller::{AppState, Shared, Status, Trigger};

/// Point size used for every row, so the dropdown reads as one consistent block.
const FONT_SIZE: f64 = 13.0;

pub struct Ivars {
    shared: Shared,
    item: Retained<NSStatusItem>,
    /// Persistent provider sections, updated in place so an open menu stays live.
    provider_items: Vec<ProviderItems>,
    last_refresh_item: Retained<NSMenuItem>,
    /// Last `AppState.version` rendered; skip redraw when unchanged.
    last_version: Cell<Option<u64>>,
    /// Spinner rotation in radians, advanced each tick while a fetch is in flight.
    spin_phase: Cell<f64>,
}

pub struct ProviderItems {
    header: Retained<NSMenuItem>,
    usage: Vec<Retained<NSMenuItem>>,
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
);

/// The "Refresh now" row: a custom view so clicking it keeps the menu open (a
/// plain menu item would dismiss it before the live result could show) and so we
/// can draw the native hover highlight that an actionless view wouldn't get.
struct RefreshRowIvars {
    trigger: Trigger,
    hovered: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "AIUsageRefreshRow"]
    #[ivars = RefreshRowIvars]
    struct RefreshRow;

    impl RefreshRow {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let bounds = self.bounds();
            let hovered = self.ivars().hovered.get();
            if hovered {
                // The same rounded accent fill macOS uses for a hovered item.
                let rect = NSRect::new(
                    NSPoint::new(5.0, 1.0),
                    NSSize::new(
                        (bounds.size.width - 10.0).max(0.0),
                        (bounds.size.height - 2.0).max(0.0),
                    ),
                );
                NSColor::selectedContentBackgroundColor().setFill();
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, 5.0, 5.0).fill();
            }
            let color = if hovered {
                NSColor::selectedMenuItemTextColor()
            } else {
                NSColor::labelColor()
            };
            let title = attributed(vec![(
                "Refresh now".into(),
                color,
                NSFont::menuFontOfSize(FONT_SIZE),
            )]);
            let size = title.size();
            // Indent to line up with the menu item rows; vertically centered.
            let y = (bounds.size.height - size.height) / 2.0;
            title.drawAtPoint(NSPoint::new(20.0, y));
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            self.ivars().hovered.set(true);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.ivars().hovered.set(false);
            self.setNeedsDisplay(true);
        }

        // Claim the mouse session so mouseUp is delivered here.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {}

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            request_refresh(&self.ivars().trigger);
        }
    }
);

/// Broadcast an explicit request. Each provider still honors its own minimum
/// gap and any active 429 cooldown; its worker raises the spinner when I/O
/// actually begins.
fn request_refresh(trigger: &Trigger) {
    trigger.fire();
}

impl Controller {
    fn refresh(&self) {
        let mtm = MainThreadMarker::from(self);
        // Snapshot under one lock: whether any provider is in flight, and (only
        // when the version changed) a fresh render. Each worker bumps the version
        // when its own I/O starts and completes.
        let (refreshing, view) = {
            let state = self.ivars().shared.lock().expect("state lock");
            let view = if self.ivars().last_version.get() == Some(state.version) {
                None
            } else {
                self.ivars().last_version.set(Some(state.version));
                Some(render(&state))
            };
            (
                state.providers.iter().any(|provider| provider.refreshing),
                view,
            )
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
        // High-contrast primary text everywhere that matters — secondary/tinted
        // text washes out against the menu's translucent material. One font size
        // throughout; hierarchy comes from weight, not size or colour.
        let primary = NSColor::labelColor();
        let secondary = NSColor::secondaryLabelColor();
        let body = NSFont::menuFontOfSize(FONT_SIZE);
        let bold = NSFont::boldSystemFontOfSize(FONT_SIZE);

        let set_usage = |item: &NSMenuItem, row: &Row| match row {
            Row::Usage {
                label: name,
                pct,
                reset,
            } => {
                let mut runs = vec![
                    (format!("  {name}   "), primary.clone(), body.clone()),
                    (format!("{pct:.0}%   "), primary.clone(), bold.clone()),
                ];
                // The window may have no reset time (endpoint returns null).
                if let Some(reset) = reset {
                    runs.push((format!("resets {reset}"), secondary.clone(), body.clone()));
                }
                item.setAttributedTitle(Some(&attributed(runs)));
            }
            Row::Note(text) => {
                let title = attributed(vec![(format!("  {text}"), primary.clone(), body.clone())]);
                item.setAttributedTitle(Some(&title));
            }
        };

        for (items, provider) in self.ivars().provider_items.iter().zip(&view.providers) {
            let suffix = match provider.status {
                Status::Stale => " · stale",
                Status::AuthExpired => " · auth required",
                Status::Error => " · unavailable",
                _ => "",
            };
            let title = attributed(vec![(
                format!("{}{suffix}", provider.name),
                primary.clone(),
                bold.clone(),
            )]);
            items.header.setAttributedTitle(Some(&title));

            for (index, item) in items.usage.iter().enumerate() {
                if let Some(row) = provider.rows.get(index) {
                    item.setHidden(false);
                    set_usage(item, row);
                } else {
                    item.setHidden(true);
                }
            }
        }

        let last = attributed(vec![(view.last_refresh.clone(), secondary, body)]);
        self.ivars()
            .last_refresh_item
            .setAttributedTitle(Some(&last));
    }
}

/// Build the status item, controller, dropdown and refresh timer. The returned
/// `Controller` (and the status item it holds) must be kept alive.
pub fn install(mtm: MainThreadMarker, shared: Shared, trigger: Trigger) -> Retained<Controller> {
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = item.button(mtm) {
        button.setTitle(&NSString::from_str("…"));
    }

    let provider_names: Vec<String> = shared
        .lock()
        .expect("state lock")
        .providers
        .iter()
        .map(|provider| provider.name.clone())
        .collect();
    let provider_items: Vec<ProviderItems> = provider_names
        .iter()
        .map(|_| ProviderItems {
            header: NSMenuItem::new(mtm),
            // Both current providers expose a five-hour and a weekly window.
            // Keeping these rows persistent lets an open menu update in place.
            usage: vec![NSMenuItem::new(mtm), NSMenuItem::new(mtm)],
        })
        .collect();
    let last_refresh_item = NSMenuItem::new(mtm);

    let controller = {
        let this = mtm.alloc::<Controller>().set_ivars(Ivars {
            shared: shared.clone(),
            item: item.clone(),
            provider_items,
            last_refresh_item: last_refresh_item.clone(),
            last_version: Cell::new(None),
            spin_phase: Cell::new(0.0),
        });
        let this: Retained<Controller> = unsafe { msg_send![super(this), init] };
        this
    };

    let menu = NSMenu::new(mtm);
    // We manage enablement ourselves so info rows keep their attributed colours
    // (auto-enable greys out actionless items).
    menu.setAutoenablesItems(false);

    for (index, provider) in controller.ivars().provider_items.iter().enumerate() {
        if index > 0 {
            menu.addItem(&NSMenuItem::separatorItem(mtm));
        }
        provider.header.setEnabled(true);
        menu.addItem(&provider.header);
        for item in &provider.usage {
            item.setEnabled(true); // keep full-contrast attributed text
            menu.addItem(item);
        }
    }

    menu.addItem(&NSMenuItem::separatorItem(mtm));
    last_refresh_item.setEnabled(false);
    menu.addItem(&last_refresh_item);
    menu.addItem(&NSMenuItem::separatorItem(mtm));

    // Custom Refresh row (keeps the menu open + native hover highlight).
    let refresh_row = {
        let this = mtm.alloc::<RefreshRow>().set_ivars(RefreshRowIvars {
            trigger,
            hovered: Cell::new(false),
        });
        let this: Retained<RefreshRow> = unsafe { msg_send![super(this), init] };
        this
    };
    refresh_row.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(240.0, 22.0),
    ));
    // Stretch to the menu's full content width so the hover highlight spans it.
    refresh_row.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    let tracking = unsafe {
        NSTrackingArea::initWithRect_options_owner_userInfo(
            NSTrackingArea::alloc(),
            refresh_row.bounds(),
            NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect,
            Some(&refresh_row),
            None,
        )
    };
    refresh_row.addTrackingArea(&tracking);
    let refresh_item = NSMenuItem::new(mtm);
    refresh_item.setView(Some(&refresh_row));
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
    /// Highest five-hour session % for the gauge (`None` when there's no data).
    session_frac: Option<f64>,
    /// Menu-bar title text, e.g. `Claude 27%` or `GPT 41%`.
    bar_title: String,
    providers: Vec<ProviderView>,
    /// The "Last refresh …" status line.
    last_refresh: String,
}

struct ProviderView {
    name: String,
    status: Status,
    /// One or two usage rows, or a single note row when there's no data.
    rows: Vec<Row>,
}

enum Row {
    Usage {
        label: String,
        pct: f64,
        reset: Option<String>,
    },
    Note(String),
}

/// Produce the `View` for the current state.
fn render(state: &AppState) -> View {
    let mut winner: Option<(&str, f64, Status)> = None;
    for provider in &state.providers {
        let Some(session) = provider
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.windows.first())
        else {
            continue;
        };
        if winner.is_none_or(|(_, pct, _)| session.utilization > pct) {
            winner = Some((&provider.name, session.utilization, provider.status));
        }
    }

    let (session_frac, bar_title) = match winner {
        Some((name, pct, status)) => {
            let stale = if status == Status::Stale { " ·" } else { "" };
            (Some(pct), format!("{name} {pct:.0}%{stale}"))
        }
        None if state
            .providers
            .iter()
            .any(|provider| provider.status == Status::Loading) =>
        {
            (None, "…".into())
        }
        None => (None, "—".into()),
    };

    let providers = state.providers.iter().map(render_provider).collect();
    View {
        session_frac,
        bar_title,
        providers,
        last_refresh: last_refresh_line(state),
    }
}

fn render_provider(provider: &crate::poller::ProviderState) -> ProviderView {
    let rows = match &provider.snapshot {
        Some(snapshot) => snapshot
            .windows
            .iter()
            .map(|window| Row::Usage {
                label: window.label.clone(),
                pct: window.utilization,
                reset: window.resets_at.map(fmt_reset),
            })
            .collect(),
        None => {
            let note = match provider.status {
                Status::Loading => "Loading…",
                Status::AuthExpired => "Signed out — run /login in Claude Code",
                Status::Error if provider.name == "GPT" => "No usage yet — run a Codex turn",
                Status::Error => "No data — refresh failed",
                Status::Stale | Status::Ok => "No data yet",
            };
            vec![Row::Note(note.into())]
        }
    };
    ProviderView {
        name: provider.name.clone(),
        status: provider.status,
        rows,
    }
}

/// Per-provider completion times make independent event-driven refreshes
/// visible: a Codex turn can update GPT without pretending Claude was checked.
fn last_refresh_line(state: &AppState) -> String {
    state
        .providers
        .iter()
        .map(|provider| {
            let when = provider
                .checked_at
                .map_or_else(|| "—".into(), |time| time.format("%H:%M:%S").to_string());
            let status = if provider.refreshing {
                "checking…"
            } else {
                match provider.status {
                    Status::Loading => "waiting",
                    Status::Ok => "OK",
                    Status::AuthExpired => "auth expired",
                    Status::Stale | Status::Error
                        if provider
                            .message
                            .as_deref()
                            .is_some_and(|message| message.contains("429")) =>
                    {
                        "rate-limited"
                    }
                    Status::Stale => "stale",
                    Status::Error => "unavailable",
                }
            };
            format!("{} {when} {status}", provider.name)
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Build a single attributed string from colored/fonted runs. Main thread only.
fn attributed(
    runs: Vec<(String, Retained<NSColor>, Retained<NSFont>)>,
) -> Retained<NSAttributedString> {
    let out = NSMutableAttributedString::new();
    for (text, color, font) in runs {
        let piece = NSMutableAttributedString::initWithString(
            NSMutableAttributedString::alloc(),
            &NSString::from_str(&text),
        );
        let range = NSRange::new(0, piece.length());
        unsafe {
            piece.addAttribute_value_range(NSForegroundColorAttributeName, &color, range);
            piece.addAttribute_value_range(NSFontAttributeName, &font, range);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poller::ProviderState;
    use crate::provider::{UsageSnapshot, UsageWindow};

    fn provider(name: &str, session: f64, weekly: f64) -> ProviderState {
        ProviderState {
            name: name.into(),
            snapshot: Some(UsageSnapshot {
                windows: vec![
                    UsageWindow {
                        label: "Session (5h)".into(),
                        utilization: session,
                        resets_at: None,
                    },
                    UsageWindow {
                        label: "Weekly (7d)".into(),
                        utilization: weekly,
                        resets_at: None,
                    },
                ],
            }),
            status: Status::Ok,
            message: None,
            updated_at: None,
            checked_at: None,
            refreshing: false,
        }
    }

    #[test]
    fn status_bar_uses_highest_five_hour_provider() {
        let state = AppState {
            providers: vec![provider("GPT", 28.0, 10.0), provider("Claude", 73.0, 15.0)],
            version: 1,
        };
        let view = render(&state);
        assert_eq!(view.bar_title, "Claude 73%");
        assert_eq!(view.session_frac, Some(73.0));
        assert_eq!(view.providers.len(), 2);
    }

    #[test]
    fn available_provider_remains_visible_when_other_has_no_data() {
        let state = AppState {
            providers: vec![
                ProviderState {
                    name: "GPT".into(),
                    snapshot: None,
                    status: Status::Error,
                    message: Some("no snapshot".into()),
                    updated_at: None,
                    checked_at: None,
                    refreshing: false,
                },
                provider("Claude", 41.0, 20.0),
            ],
            version: 1,
        };
        let view = render(&state);
        assert_eq!(view.bar_title, "Claude 41%");
        assert_eq!(view.providers.len(), 2);
    }
}
