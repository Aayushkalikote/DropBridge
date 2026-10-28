// DropBridge logo: a white bridge and drop on a blue gradient.
//
//   swiftc -O -o /tmp/mkicon assets/icon.swift
//   /tmp/mkicon assets/icon.png              # rounded tile with margin (README, docs)
//   /tmp/mkicon assets/icon-full.png --full  # full-bleed square; macOS applies its own mask
//
// Use the full-bleed one for app bundles: macOS 26 puts icons with a transparent margin on a
// white plate in notifications.
import AppKit

let full = CommandLine.arguments.contains("--full")
let size = 1024.0
// Apple's macOS icon grid: an 824 pt tile centred in 1024.
let (inset, radius, glyphScale) = full ? (0.0, 0.0, 1.2) : (100.0, 185.0, 1.0)
let side = size - 2 * inset

let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size), pixelsHigh: Int(size), bitsPerSample: 8,
    samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
let ctx = NSGraphicsContext.current!.cgContext

let tile = CGPath(roundedRect: CGRect(x: inset, y: inset, width: side, height: side),
    cornerWidth: radius, cornerHeight: radius, transform: nil)
if !full {
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -10), blur: 28, color: NSColor(white: 0, alpha: 0.3).cgColor)
    ctx.addPath(tile)
    ctx.fillPath()
    ctx.restoreGState()
}
ctx.saveGState()
ctx.addPath(tile)
ctx.clip()
let blue = CGGradient(colorsSpace: CGColorSpaceCreateDeviceRGB(), colors: [
    NSColor(srgbRed: 0.45, green: 0.66, blue: 0.98, alpha: 1).cgColor,
    NSColor(srgbRed: 0.17, green: 0.38, blue: 0.86, alpha: 1).cgColor] as CFArray, locations: [0, 1])!
ctx.drawLinearGradient(blue, start: CGPoint(x: inset, y: inset + side), end: CGPoint(x: inset + side, y: inset), options: [])
ctx.restoreGState()

ctx.translateBy(x: 512, y: 512)
ctx.scaleBy(x: glyphScale, y: glyphScale)
ctx.translateBy(x: -512, y: -512)
ctx.setStrokeColor(NSColor.white.cgColor)
ctx.setFillColor(NSColor.white.cgColor)
ctx.setLineCap(.round)

// Arch and deck.
let cx = 512.0, deckY = 400.0, archR = 250.0, archY = deckY - 60
ctx.setLineWidth(44)
ctx.addArc(center: CGPoint(x: cx, y: archY), radius: archR, startAngle: .pi * 0.86, endAngle: .pi * 0.14, clockwise: true)
ctx.strokePath()
ctx.move(to: CGPoint(x: 220, y: deckY))
ctx.addLine(to: CGPoint(x: 804, y: deckY))
ctx.strokePath()

// Hangers between deck and arch.
ctx.setLineWidth(30)
for dx in [-150.0, 150.0] {
    let top = archY + (archR * archR - dx * dx).squareRoot()
    ctx.move(to: CGPoint(x: cx + dx, y: deckY))
    ctx.addLine(to: CGPoint(x: cx + dx, y: top - 10))
    ctx.strokePath()
}

// Drop, point up.
let dropR = 52.0, drop = CGPoint(x: cx, y: deckY + 80)
ctx.move(to: CGPoint(x: cx, y: drop.y + dropR * 2.1))
ctx.addLine(to: CGPoint(x: cx - dropR * 0.92, y: drop.y + dropR * 0.4))
ctx.addArc(center: drop, radius: dropR, startAngle: .pi - 0.4, endAngle: 0.4, clockwise: false)
ctx.closePath()
ctx.fillPath()

NSGraphicsContext.current = nil
let out = CommandLine.arguments.dropFirst().first { !$0.hasPrefix("--") } ?? "icon.png"
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: out))
