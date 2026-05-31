//! A small pie gauge drawn as a template `NSImage`, used in the status bar to
//! show the session window's fill at a glance. Template => it adapts to the
//! menu bar's light/dark appearance automatically.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_app_kit::{NSBezierPath, NSColor, NSImage};
use objc2_foundation::{NSPoint, NSRect, NSSize};

const SIZE: f64 = 14.0;
const INSET: f64 = 1.5;

/// A circle outline with a clockwise-from-top filled wedge for `fraction`
/// (0.0–1.0, clamped). Must be called on the main thread.
pub fn session_gauge(fraction: f64) -> Retained<NSImage> {
    let frac = fraction.clamp(0.0, 1.0);
    let size = NSSize::new(SIZE, SIZE);
    let center = NSPoint::new(SIZE / 2.0, SIZE / 2.0);
    let radius = (SIZE - 2.0 * INSET) / 2.0;
    let oval = NSRect::new(
        NSPoint::new(INSET, INSET),
        NSSize::new(SIZE - 2.0 * INSET, SIZE - 2.0 * INSET),
    );

    let handler = RcBlock::new(move |_rect: NSRect| -> Bool {
        // Full-circle track outline.
        NSColor::blackColor().setStroke();
        let track = NSBezierPath::bezierPath();
        track.appendBezierPathWithOvalInRect(oval);
        track.setLineWidth(1.0);
        track.stroke();

        // Filled wedge from 12 o'clock, clockwise, proportional to fraction.
        if frac > 0.0 {
            NSColor::blackColor().setFill();
            let wedge = NSBezierPath::bezierPath();
            wedge.moveToPoint(center);
            wedge.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
                center,
                radius,
                90.0,
                90.0 - 360.0 * frac,
                true,
            );
            wedge.closePath();
            wedge.fill();
        }
        Bool::YES
    });

    let image = NSImage::imageWithSize_flipped_drawingHandler(size, false, &handler);
    image.setTemplate(true);
    image
}
