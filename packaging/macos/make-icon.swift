// Draws the tuxDrop app icon into an .iconset folder (for iconutil): the
// menu bar drop with its arrow, white on a blue rounded square.
//   make-icon <out.iconset>

import AppKit

let out = URL(fileURLWithPath: CommandLine.arguments[1])
try FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)

func draw(size: Int) -> Data {
    let s = CGFloat(size)
    let ctx = CGContext(
        data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0,
        space: CGColorSpace(name: CGColorSpace.sRGB)!,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    // y down, in units of the icon size, as the menu bar icon.
    ctx.translateBy(x: 0, y: s)
    ctx.scaleBy(x: s, y: -s)

    // macOS icon grid: the body is 824/1024 of the canvas.
    let body = CGRect(x: 100, y: 100, width: 824, height: 824).applying(CGAffineTransform(scaleX: 1 / 1024, y: 1 / 1024))
    let shape = CGPath(roundedRect: body, cornerWidth: 0.18, cornerHeight: 0.18, transform: nil)
    ctx.saveGState()
    ctx.addPath(shape)
    ctx.clip()
    let colors = [CGColor(srgbRed: 0.33, green: 0.60, blue: 0.98, alpha: 1),
                  CGColor(srgbRed: 0.11, green: 0.31, blue: 0.85, alpha: 1)] as CFArray
    let gradient = CGGradient(colorsSpace: CGColorSpace(name: CGColorSpace.sRGB), colors: colors, locations: [0, 1])!
    ctx.drawLinearGradient(gradient, start: CGPoint(x: 0.5, y: body.minY), end: CGPoint(x: 0.5, y: body.maxY), options: [])
    ctx.restoreGState()

    // The drop in a square of the body, same geometry as drop_pixel in
    // crates/tuxdropd/src/platform/native/tray.rs.
    let inset: CGFloat = 0.17
    let box = body.insetBy(dx: body.width * inset, dy: body.height * inset)
    func p(_ x: CGFloat, _ y: CGFloat) -> CGPoint { CGPoint(x: box.minX + x * box.width, y: box.minY + y * box.height) }
    let (cx, cy, r, tip): (CGFloat, CGFloat, CGFloat, CGFloat) = (0.5, 0.62, 0.32, 0.06)
    let a = acos(r / (cy - tip))
    let up = -CGFloat.pi / 2
    let left = p(cx + r * cos(up - a), cy + r * sin(up - a))
    let right = p(cx + r * cos(up + a), cy + r * sin(up + a))

    ctx.beginTransparencyLayer(auxiliaryInfo: nil)
    ctx.setFillColor(CGColor(gray: 1, alpha: 1))
    ctx.addEllipse(in: CGRect(origin: p(cx - r, cy - r), size: CGSize(width: 2 * r * box.width, height: 2 * r * box.height)))
    ctx.fillPath()
    ctx.addLines(between: [p(cx, tip), left, p(cx, cy), right])
    ctx.closePath()
    ctx.fillPath()
    // Arrow cut out of it.
    ctx.setBlendMode(.clear)
    ctx.fill(CGRect(origin: p(cx - 0.045, 0.42), size: CGSize(width: 0.09 * box.width, height: 0.22 * box.height)))
    ctx.addLines(between: [p(cx - 0.17, 0.62), p(cx + 0.17, 0.62), p(cx, 0.82)])
    ctx.closePath()
    ctx.fillPath()
    ctx.endTransparencyLayer()

    let rep = NSBitmapImageRep(cgImage: ctx.makeImage()!)
    return rep.representation(using: .png, properties: [:])!
}

for points in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let suffix = scale == 1 ? "" : "@2x"
        try draw(size: points * scale).write(to: out.appendingPathComponent("icon_\(points)x\(points)\(suffix).png"))
    }
}
