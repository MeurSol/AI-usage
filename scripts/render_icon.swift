// Renders the AI-usage app icon: an Apple "activity ring" style gauge — a dark
// squircle with a green→teal gradient ring (~72%) — into a .iconset directory.
// Run via scripts/make-icon.sh.
import AppKit

let FRACTION: CGFloat = 0.72

func rgb(_ r: CGFloat, _ g: CGFloat, _ b: CGFloat, _ a: CGFloat = 1) -> NSColor {
    NSColor(srgbRed: r / 255, green: g / 255, blue: b / 255, alpha: a)
}
func lerp(_ c0: NSColor, _ c1: NSColor, _ t: CGFloat) -> NSColor {
    let a = c0.usingColorSpace(.sRGB)!, b = c1.usingColorSpace(.sRGB)!
    return NSColor(
        srgbRed: a.redComponent + (b.redComponent - a.redComponent) * t,
        green: a.greenComponent + (b.greenComponent - a.greenComponent) * t,
        blue: a.blueComponent + (b.blueComponent - a.blueComponent) * t, alpha: 1)
}
// macOS-style squircle (continuous rounded corners approximated).
func squircle(_ rect: NSRect) -> NSBezierPath {
    NSBezierPath(roundedRect: rect, xRadius: rect.width * 0.2237, yRadius: rect.height * 0.2237)
}
// Gradient ring with rounded ends, clockwise from 12 o'clock, sweep = frac.
func gradientRing(center: NSPoint, radius: CGFloat, lineWidth: CGFloat,
                  frac: CGFloat, c0: NSColor, c1: NSColor) {
    let steps = 160
    let sweep = 360 * frac
    for i in 0..<steps {
        let t0 = CGFloat(i) / CGFloat(steps), t1 = CGFloat(i + 1) / CGFloat(steps)
        let seg = NSBezierPath()
        seg.appendArc(withCenter: center, radius: radius,
                      startAngle: 90 - sweep * t0, endAngle: 90 - sweep * t1, clockwise: true)
        seg.lineWidth = lineWidth
        seg.lineCapStyle = .round
        lerp(c0, c1, t0).setStroke()
        seg.stroke()
    }
}

func renderPNG(_ size: Int) -> Data {
    let n = CGFloat(size)
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size,
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)

    let pad = n * 0.085
    let tile = NSRect(x: pad, y: pad, width: n - 2 * pad, height: n - 2 * pad)
    let center = NSPoint(x: n / 2, y: n / 2)

    // Dark squircle background with a subtle top→bottom gradient.
    let sq = squircle(tile)
    sq.addClip()
    NSGradient(colors: [rgb(38, 38, 42), rgb(8, 8, 10)])!.draw(in: sq, angle: -90)

    // Faint full-circle track (the "remaining" hint) + green→teal progress ring.
    let r = tile.width * 0.30
    let lw = tile.width * 0.135
    let track = NSBezierPath()
    track.appendArc(withCenter: center, radius: r, startAngle: 0, endAngle: 360)
    track.lineWidth = lw
    rgb(48, 209, 88, 0.16).setStroke()
    track.stroke()
    gradientRing(center: center, radius: r, lineWidth: lw, frac: FRACTION,
                 c0: rgb(48, 209, 88), c1: rgb(48, 205, 209))

    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

guard CommandLine.arguments.count > 1 else {
    FileHandle.standardError.write("usage: render_icon.swift <iconset-dir>\n".data(using: .utf8)!)
    exit(1)
}
let outDir = CommandLine.arguments[1]
let entries: [(Int, String)] = [
    (16, "icon_16x16.png"), (32, "icon_16x16@2x.png"),
    (32, "icon_32x32.png"), (64, "icon_32x32@2x.png"),
    (128, "icon_128x128.png"), (256, "icon_128x128@2x.png"),
    (256, "icon_256x256.png"), (512, "icon_256x256@2x.png"),
    (512, "icon_512x512.png"), (1024, "icon_512x512@2x.png"),
]
for (px, name) in entries {
    try! renderPNG(px).write(to: URL(fileURLWithPath: "\(outDir)/\(name)"))
}
print("wrote \(entries.count) icon images to \(outDir)")
