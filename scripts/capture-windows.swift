// Screenshot of only the given windows, composited in their on-screen order
// (e.g. the app's main window plus its overlay child window), even when other
// apps' windows cover them. Nothing else on screen is captured.
//
//   swift scripts/capture-windows.swift <out.png> <window-number>...
//
// The first window's frame is the crop. Uses ScreenCaptureKit, so it needs
// the Screen Recording permission like `screencapture`.

import CoreGraphics
import Foundation
import ImageIO
import ScreenCaptureKit
import UniformTypeIdentifiers

let args = CommandLine.arguments
guard args.count >= 3 else {
    FileHandle.standardError.write("usage: capture-windows.swift <out.png> <window-number>...\n".data(using: .utf8)!)
    exit(2)
}
let out = URL(fileURLWithPath: args[1])
let ids = args.dropFirst(2).compactMap { UInt32($0) }

let done = DispatchSemaphore(value: 0)
var status: Int32 = 1
Task {
    defer { done.signal() }
    do {
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        let windows = ids.compactMap { id in content.windows.first { $0.windowID == id } }
        guard let first = windows.first,
              let display = content.displays.first(where: { $0.frame.intersects(first.frame) }) ?? content.displays.first
        else {
            FileHandle.standardError.write("windows not on screen: \(ids)\n".data(using: .utf8)!)
            return
        }
        let filter = SCContentFilter(display: display, including: windows)
        let config = SCStreamConfiguration()
        let scale = CGFloat(filter.pointPixelScale)
        let crop = first.frame.offsetBy(dx: -display.frame.origin.x, dy: -display.frame.origin.y)
        config.sourceRect = crop
        config.width = Int(crop.width * scale)
        config.height = Int(crop.height * scale)
        config.showsCursor = false
        let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
        guard let dest = CGImageDestinationCreateWithURL(out as CFURL, UTType.png.identifier as CFString, 1, nil) else { return }
        CGImageDestinationAddImage(dest, image, nil)
        if CGImageDestinationFinalize(dest) { status = 0 }
    } catch {
        FileHandle.standardError.write("capture failed: \(error)\n".data(using: .utf8)!)
    }
}
done.wait()
exit(status)
