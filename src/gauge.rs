//! A small pie gauge drawn as a colored `NSImage`, used in the status bar to
//! show the session window's fill at a glance. The fill is tinted by a
//! green→red ramp so the utilization level is readable at a glance.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_app_kit::{NSBezierPath, NSColor, NSImage};
use objc2_foundation::{NSPoint, NSRect, NSSize};

const SIZE: f64 = 14.0;
const INSET: f64 = 1.5;

/// Fill tint for a utilization fraction: green (low) → red (high).
pub fn level_color(fraction: f64) -> Retained<NSColor> {
    let f = fraction.clamp(0.0, 1.0);
    let hue = (1.0 - f) * 0.33; // 0.33 ≈ green, 0.0 = red
    NSColor::colorWithHue_saturation_brightness_alpha(hue, 0.85, 0.9, 1.0)
}

/// A circle with a clockwise-from-top filled wedge for `fraction` (0.0–1.0,
/// clamped), tinted by `level_color`. A neutral track ring stays visible on
/// both light and dark menu bars. Must be called on the main thread.
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
        // Neutral track ring (gray reads on both light and dark bars).
        NSColor::colorWithWhite_alpha(0.55, 0.9).setStroke();
        let track = NSBezierPath::bezierPath();
        track.appendBezierPathWithOvalInRect(oval);
        track.setLineWidth(1.0);
        track.stroke();

        // Colored wedge from 12 o'clock, clockwise, proportional to fraction.
        if frac > 0.0 {
            level_color(frac).setFill();
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

    // Not a template image — we want the actual colors to show.
    NSImage::imageWithSize_flipped_drawingHandler(size, false, &handler)
}
