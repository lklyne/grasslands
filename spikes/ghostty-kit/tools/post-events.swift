// Posts real keyboard and mouse events to one process only (CGEventPostToPid),
// so they can never land in another app.
//   swift post-events.swift <pid> type "<text>"
//   swift post-events.swift <pid> drag <x0> <y0> <x1> <y1> <steps> <pause-ms-at-end>
import CoreGraphics
import Foundation

let args = CommandLine.arguments
let pid = pid_t(args[1])!
let source = CGEventSource(stateID: .hidSystemState)

func key(_ code: CGKeyCode, _ text: String?) {
    for down in [true, false] {
        let event = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down)!
        if let text = text {
            let units = Array(text.utf16)
            event.keyboardSetUnicodeString(stringLength: units.count, unicodeString: units)
        }
        event.postToPid(pid)
        usleep(15_000)
    }
}

// US ANSI keycodes for the characters the spike types.
let codes: [Character: CGKeyCode] = [
    "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9,
    "b": 11, "q": 12, "w": 13, "e": 14, "r": 15, "y": 16, "t": 17, "1": 18, "2": 19,
    "3": 20, "4": 21, "6": 22, "5": 23, "9": 25, "7": 26, "-": 27, "8": 28, "0": 29,
    "o": 31, "u": 32, "i": 34, "p": 35, "l": 37, "j": 38, "k": 40, "n": 45, "m": 46,
    " ": 49,
]

func mouse(_ type: CGEventType, _ x: Double, _ y: Double) {
    let event = CGEvent(mouseEventSource: source, mouseType: type,
                        mouseCursorPosition: CGPoint(x: x, y: y), mouseButton: .left)!
    event.postToPid(pid)
}

switch args[2] {
case "type":
    for ch in args[3] {
        if ch == "\n" { key(36, nil) } else if let code = codes[ch] { key(code, String(ch)) }
    }
case "drag":
    let (x0, y0, x1, y1) = (Double(args[3])!, Double(args[4])!, Double(args[5])!, Double(args[6])!)
    let steps = Int(args[7])!
    mouse(.mouseMoved, x0, y0); usleep(100_000)
    mouse(.leftMouseDown, x0, y0); usleep(100_000)
    for i in 1...steps {
        let t = Double(i) / Double(steps)
        mouse(.leftMouseDragged, x0 + (x1 - x0) * t, y0 + (y1 - y0) * t)
        usleep(16_000)
    }
    usleep(UInt32(args[8])! * 1000)
    mouse(.leftMouseUp, x1, y1)
default:
    print("unknown command")
}
