#if DEBUG
import AppKit
import SwiftUI

/// Reduce Motion checks without a screen capture. `motionSample=NAME:X:Y:MS` samples what moves every frame for MS
/// milliseconds: the scroll view under X:Y, the table's drop slot and rows, and the expanded player's clip.
/// `motionReport=NAME` reports how many distinct values each passed through. A glide passes through many, and
/// under reduced motion each jumps in one step.
@MainActor enum MotionCheck {
    /// Set by the expanded player's clip as SwiftUI interpolates it.
    static var clipProgress: Double?
    private static var samples: [String: [String: [(Double, String)]]] = [:]
    private static var timer: Timer?
    private static var running: (name: String, start: Double)?

    /// A value a view reports as SwiftUI animates it (the lyric lines' move), while a sampling run is on.
    static func note(_ probe: String, _ value: Double) {
        guard let running else { return }
        record(running.name, probe, CACurrentMediaTime() - running.start, String(format: "%.1f", value))
    }

    static func start(_ value: String, window: NSWindow) {
        let parts = value.split(separator: ":").map(String.init)
        guard parts.count == 4, let x = Double(parts[1]), let y = Double(parts[2]), let ms = Double(parts[3]) else { return }
        let name = parts[0]
        let point = NSPoint(x: x, y: (window.contentView?.bounds.height ?? 0) - y)
        let root = window.contentView?.superview
        var hit = root.flatMap { $0.hitTest($0.convert(point, from: nil)) }
        while let view = hit, !(view is NSScrollView) { hit = view.superview }
        let scroll = hit as? NSScrollView
        samples[name] = [:]
        if let scroll { record(name, "scrollView", 0, "\(scroll.convert(scroll.bounds, to: nil)) doc \(scroll.documentView?.frame.height ?? 0)") }
        let start = CACurrentMediaTime()
        running = (name, start)
        clipProgress = nil
        timer?.invalidate()
        let ticker = Timer(timeInterval: 1.0 / 120, repeats: true) { _ in
            MainActor.assumeIsolated {
                let t = CACurrentMediaTime() - start
                if t * 1000 > ms { MotionCheck.timer?.invalidate(); MotionCheck.timer = nil; MotionCheck.running = nil; return }
                if let scroll { record(name, "scroll", t, String(format: "%.1f", scroll.contentView.bounds.minY)) }
                if let root, let slot = find(DropSlotView.self, in: root), !slot.isHidden {
                    let layer = slot.layer?.presentation() ?? slot.layer
                    if let layer { record(name, "slot", t, String(format: "%.1f %.1f %.2f", layer.frame.minY, layer.frame.height, layer.opacity)) }
                }
                if let root, let table = find(NSTableView.self, in: root) {
                    let range = table.rows(in: table.visibleRect)
                    let ys = (range.location..<min(range.location + 8, range.location + range.length)).compactMap { row in
                        table.rowView(atRow: row, makeIfNecessary: false).flatMap { ($0.layer?.presentation() ?? $0.layer).map { String(format: "%.0f", $0.frame.minY) } }
                    }
                    record(name, "tableRows", t, ys.joined(separator: " "))
                }
                // presentation layers, bottom edges up from the scroll view's bottom: a blurred lyric line has its own layer.
                if let scroll, let top = scroll.layer {
                    let shownTop = top.presentation() ?? top
                    var ys: [String] = []
                    func walk(_ layer: CALayer) {
                        if !(layer.delegate is NSView) && layer.sublayers == nil {
                            let shown = layer.presentation() ?? layer
                            let f = shown.convert(shown.bounds, to: shownTop)
                            if f.maxY > 0 && f.minY < shownTop.bounds.height { ys.append(String(format: "%.1f", shownTop.bounds.height - f.maxY)) }
                        }
                        for sub in layer.sublayers ?? [] { walk(sub) }
                    }
                    walk(top)
                    record(name, "drawn", t, ys.joined(separator: " "))
                }
                if let clipProgress { record(name, "expandClip", t, String(format: "%.3f", clipProgress)) }
                for view in LyricFillView.alive.allObjects where view.window != nil {
                    let model = view.convert(view.bounds, to: nil).minY
                    var drawn: CGFloat = 0, layer: CALayer? = view.layer
                    while let l = layer, l.superlayer != nil { drawn += (l.presentation() ?? l).frame.minY; layer = l.superlayer }
                    record(name, "fill", t, String(format: "%.1f %.1f %@", model, drawn, view.built?.text.prefix(12).description ?? "-"))
                }
            }
        }
        timer = ticker
        RunLoop.main.add(ticker, forMode: .common)
    }

    /// `expandAnimated=1|0` opens or closes the expanded player the way its buttons do.
    static func expand(_ shell: ShellModel, _ on: Bool) {
        withAnimation(ExpandClip.animation) { shell.expanded = on }
    }

    /// every sample of one probe, `[ms, value]`.
    static func series(_ name: String, _ probe: String) -> [[Any]] {
        (samples[name]?[probe] ?? []).map { [Int($0.0 * 1000), $0.1] }
    }

    static func report(_ name: String) -> [String: Any] {
        var out: [String: Any] = ["motionReport": name, "reduced": Motion.isReduced]
        for (probe, values) in samples[name] ?? [:] {
            var distinct: [String] = []
            for (_, v) in values where distinct.last != v { distinct.append(v) }
            let changes = zip(values, values.dropFirst()).filter { $0.1 != $1.1 }.map { $1.0 }
            out[probe] = ["steps": distinct.count, "first": distinct.first ?? "", "last": distinct.last ?? "",
                          "changingMs": Int(((changes.last ?? 0) - (changes.first ?? 0)) * 1000), "samples": values.count]
        }
        return out
    }

    private static func record(_ name: String, _ probe: String, _ t: Double, _ value: String) {
        samples[name, default: [:]][probe, default: []].append((t, value))
    }

    private static func find<V: NSView>(_ type: V.Type, in view: NSView) -> V? {
        if let match = view as? V { return match }
        for sub in view.subviews { if let match = find(type, in: sub) { return match } }
        return nil
    }
}

/// `-naviaFakeWordTiming YES`: spreads each synced line's words evenly, on a server without word timing. never in release.
enum WordTimingCheck {
    private static let on = UserDefaults.standard.bool(forKey: "naviaFakeWordTiming")

    static func lines(_ lines: [LyricLine]) -> [LyricLine] {
        guard on else { return lines }
        return lines.enumerated().map { i, line in
            guard let start = line.start, line.words.isEmpty else { return line }
            let end = i + 1 < lines.count ? (lines[i + 1].start ?? start + 3000) : start + 3000
            var words: [LyricWord] = []
            var offset = 0
            let parts = line.text.split(separator: " ", omittingEmptySubsequences: false)
            let step = Double(end - start) / Double(max(1, parts.count))
            for (k, part) in parts.enumerated() {
                let bytes = part.utf8.count
                let from = UInt32(start) + UInt32(Double(k) * step), to = UInt32(start) + UInt32(Double(k + 1) * step)
                words.append(LyricWord(start: UInt32(offset), end: UInt32(offset + bytes), startMs: from, endMs: to))
                offset += bytes + 1
            }
            var timed = line
            timed.words = words
            timed.endMs = end
            return timed
        }
    }
}

#endif
