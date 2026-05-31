// Renders the AI-usage app icon: a colored ring gauge at ~40% on a dark
// rounded-square, into a .iconset directory. Run via scripts/make-icon.sh.
import AppKit

let FRACTION: CGFloat = 0.40

func levelColor(_ f: CGFloat) -> NSColor {
    let c = min(max(f, 0), 1)
    let hue = (1 - c) * 0.33 // 0.33 ≈ green, 0.0 = red
    return NSColor(hue: hue, saturation: 0.85, brightness: 0.95, alpha: 1)
}

func renderPNG(_ size: Int) -> Data {
    let n = CGFloat(size)
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size,
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)

    // Dark rounded-square background.
    let pad = n * 0.055
    let bgRect = NSRect(x: pad, y: pad, width: n - 2 * pad, height: n - 2 * pad)
    let bg = NSBezierPath(roundedRect: bgRect, xRadius: n * 0.225, yRadius: n * 0.225)
    NSColor(calibratedRed: 0.137, green: 0.149, blue: 0.176, alpha: 1).setFill()
    bg.fill()

    // Ring gauge.
    let center = NSPoint(x: n / 2, y: n / 2)
    let lw = n * 0.12
    let radius = (n - 2 * pad) / 2 - lw * 0.95

    let track = NSBezierPath()
    track.appendArc(withCenter: center, radius: radius, startAngle: 0, endAngle: 360)
    track.lineWidth = lw
    NSColor(calibratedWhite: 0.30, alpha: 1).setStroke()
    track.stroke()

    let prog = NSBezierPath()
    prog.appendArc(withCenter: center, radius: radius,
                   startAngle: 90, endAngle: 90 - 360 * FRACTION, clockwise: true)
    prog.lineWidth = lw
    prog.lineCapStyle = .round
    levelColor(FRACTION).setStroke()
    prog.stroke()

    // "40%" label for the larger sizes (unreadable below ~128px).
    if size >= 128 {
        let label = "\(Int(FRACTION * 100))%"
        let fontSize = n * 0.26
        let attrs: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: fontSize, weight: .semibold),
            .foregroundColor: NSColor.white,
        ]
        let str = NSAttributedString(string: label, attributes: attrs)
        let sz = str.size()
        str.draw(at: NSPoint(x: center.x - sz.width / 2, y: center.y - sz.height / 2))
    }

    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

guard CommandLine.arguments.count > 1 else {
    FileHandle.standardError.write("usage: render_icon.swift <iconset-dir>\n".data(using: .utf8)!)
    exit(1)
}
let outDir = CommandLine.arguments[1]
// (pixel size, filename) entries an .iconset needs.
let entries: [(Int, String)] = [
    (16, "icon_16x16.png"), (32, "icon_16x16@2x.png"),
    (32, "icon_32x32.png"), (64, "icon_32x32@2x.png"),
    (128, "icon_128x128.png"), (256, "icon_128x128@2x.png"),
    (256, "icon_256x256.png"), (512, "icon_256x256@2x.png"),
    (512, "icon_512x512.png"), (1024, "icon_512x512@2x.png"),
]
for (px, name) in entries {
    let data = renderPNG(px)
    try! data.write(to: URL(fileURLWithPath: "\(outDir)/\(name)"))
}
print("wrote \(entries.count) icon images to \(outDir)")
