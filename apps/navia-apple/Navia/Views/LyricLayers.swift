import QuartzCore
import SwiftUI

/// the sweep, word lifts and held-word glow are layer animations on the song's time, so the window server runs them
/// and the app draws nothing while the line is sung. text and glow images come from `LyricSweepRenderer`.
struct LyricFill: NSViewRepresentable {
    let text: String
    let size: CGFloat
    /// the text's visual lines as laid out on screen.
    let lines: [LyricFillLine]
    /// the text's top left in this view.
    let origin: CGPoint
    /// the width the text was offered.
    let width: CGFloat
    let timing: LyricTiming
    let lift: CGFloat
    /// counted from the end of the text.
    let lastWord: Int
    /// spaces after the last word.
    let trailing: Int

    func makeNSView(context: Context) -> LyricFillView { LyricFillView() }

    func updateNSView(_ view: LyricFillView, context: Context) {
        view.environment = context.environment
        view.show(LyricFillView.Content(text: text, size: size, lines: lines, origin: origin, width: width, lift: lift, lastWord: lastWord,
                                        trailing: trailing, scale: context.environment.displayScale, start: timing.start, span: timing.span, line: timing.line,
                                        hold: timing.hold, wave: timing.wave, whole: timing.whole))
        view.follow(timing)
    }
}

/// animated by Core Animation on the song's time; `GapMarker` draws the same bars until the row has settled.
struct LyricGapBars: NSViewRepresentable {
    let timing: LyricTiming
    /// the marker scales with it.
    let size: CGFloat

    func makeNSView(context: Context) -> LyricGapView { LyricGapView() }

    func updateNSView(_ view: LyricGapView, context: Context) {
        view.show(LyricGapView.Content(size: size, start: timing.start, span: timing.span))
        view.follow(timing)
    }
}

/// `clock`'s time is the song position in seconds, so animations inside begin at song time, stop on pause and jump
/// with a seek.
class LyricLayerView: NSView {
    let clock = CALayer()
    private var timing: LyricTiming?

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.addSublayer(clock)
    }

    required init?(coder: NSCoder) { nil }
    override var isFlipped: Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    #if DEBUG
    /// lets check runs see where the layers are drawn, each time AppKit moves them.
    override func setFrameOrigin(_ newOrigin: NSPoint) {
        super.setFrameOrigin(newOrigin)
        if window != nil { MotionCheck.note(probe, convert(NSPoint.zero, to: nil).y) }
    }
    var probe: String { "layerY" }
    #endif

        func follow(_ timing: LyricTiming) {
        if self.timing?.clock !== timing.clock { self.timing?.clock?.unfollow(self) }
        self.timing = timing
        timing.clock?.follow(self)
        sync()
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window == nil { timing?.clock?.unfollow(self) } else { timing?.clock?.follow(self); sync() }
    }

    /// running while the song plays, held while it doesn't.
    func sync() {
        guard let timing, let parent = clock.superlayer else { return }
        let runs = timing.playing && timing.clock != nil
        let song = runs ? timing.clock?.estimate() ?? 0 : timing.nowMs() / 1000
        if runs, clock.speed == 1, abs(clock.convertTime(CACurrentMediaTime(), from: nil) - song) < T.lyricLayerDrift { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        clock.speed = runs ? 1 : 0
        clock.beginTime = parent.convertTime(CACurrentMediaTime(), from: nil)
        clock.timeOffset = song
        CATransaction.commit()
    }

    /// spread evenly from `from` to `to` (song ms), holding its first value before and its last after.
    static func animation(_ keyPath: String, _ values: [CGFloat], from: Double, to: Double) -> CAKeyframeAnimation {
        let animation = CAKeyframeAnimation(keyPath: keyPath)
        animation.values = values.count > 1 ? values : values + values
        animation.calculationMode = .linear
        // A begin time of zero means "now" to Core Animation.
        animation.beginTime = max(0.001, from / 1000)
        animation.duration = max(0.001, (to - from) / 1000)
        animation.fillMode = .both
        animation.isRemovedOnCompletion = false
        return animation
    }

    /// `lyricLayerRate` a second between `from` and `to` ms, both ends included.
    static func times(from: Double, to: Double) -> [Double] {
        let steps = max(1, Int(((to - from) / 1000 * T.lyricLayerRate).rounded(.up)))
        return (0...steps).map { from + (to - from) * Double($0) / Double(steps) }
    }
}

final class LyricFillView: LyricLayerView {
    struct Content: Equatable {
        var text: String
        var size: CGFloat
        var lines: [LyricFillLine]
        var origin: CGPoint
        var width: CGFloat
        var lift: CGFloat
        var lastWord: Int
        var trailing: Int
        var scale: CGFloat
        var start: Int
        var span: Int
        var line: LyricLine?
        var hold: Range<Int>?
        var wave: Range<Int>?
        var whole: Bool
    }

    var environment = EnvironmentValues()
    private var content: Content?
    private(set) var built: Content?

    func show(_ content: Content) {
        #if DEBUG
        Self.alive.add(self)
        #endif
        guard content != self.content else { return }
        // SwiftUI reuses one view from line to line: clear another line's layers now, or the old line shows for a frame.
        if content.start != self.content?.start || content.text != self.content?.text { clear() }
        self.content = content
        needsLayout = true
    }

    private func clear() {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        clock.sublayers = nil
        CATransaction.commit()
        built = nil
    }

    override func layout() {
        super.layout()
        guard window != nil, let content, content != built else { return }
        built = content
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        build(content)
        CATransaction.commit()
        sync()
    }

    /// `pad` of room around it, `shift` into its pixel grid; `drawn` picks glyphs by index.
    private func image(_ c: Content, glow: Bool, pad: CGFloat, shift: CGPoint, drawn: ((Int) -> Bool)? = nil) -> CGImage? {
        let renderer = ImageRenderer(content: LyricSweepRenderer.text(c.text, size: c.size)
            .textRenderer(LyricSweepRenderer(progress: 1, active: glow, timed: c.line != nil, hold: glow ? 1 : 0, lastWord: c.lastWord,
                                             part: glow ? .glow : .all, drawn: drawn))
            .multilineTextAlignment(.leading)
            .frame(width: c.width, alignment: .leading)
            .padding(.leading, pad + shift.x).padding(.top, pad + shift.y).padding(.trailing, pad).padding(.bottom, pad)
            .environment(\.self, environment))
        renderer.scale = c.scale
        return renderer.cgImage
    }

    #if DEBUG
    /// check runs: what each fill view built, and the views alive now.
    static var builds: [String] = []
    static let alive = NSHashTable<LyricFillView>.weakObjects()
    static func census() -> [String] {
        alive.allObjects.map { v in
            "\(ObjectIdentifier(v).hashValue % 100000) inWindow \(v.window != nil) hidden \(v.isHiddenOrHasHiddenAncestor) frame \(v.window.map { _ in v.convert(v.bounds, to: nil) } ?? .zero) layers \(v.clock.sublayers?.count ?? 0) built '\(v.built?.text ?? "-")' super \(v.superview.map { String(describing: type(of: $0)) } ?? "nil")"
        }
    }
    #endif

    private func build(_ c: Content) {
        #if DEBUG
        Self.builds.append("\(ObjectIdentifier(self).hashValue % 100000) start \(c.start) span \(c.span) lines \(c.lines.count) text '\(c.text)'")
        #endif
        clock.sublayers = nil
        guard !c.lines.isEmpty, c.width > 0 else { return }
        let scale = c.scale
        let pad = (c.size * 0.75).rounded(.up)
        // the image starts on a whole pixel and the text sits as far off the grid as on screen, so glyphs land on the same
        // pixels as the text they replace.
        let aligned = backingAlignedRect(CGRect(origin: c.origin, size: CGSize(width: 1, height: 1)), options: .alignAllEdgesNearest).origin
        let shift = CGPoint(x: c.origin.x - aligned.x, y: c.origin.y - aligned.y)
        // A rolling last word's letters come from two images of their own, even and odd letters, so a letter reaching over
        // its neighbour (a kerned pair) is never cut; the line's image leaves them out.
        let count = c.text.count
        let letters = c.lift > 0 && c.wave != nil ? LyricWave.letters(in: c.text) : 0
        let rolling = count - c.trailing - letters..<count - c.trailing
        func parity(_ p: Int) -> (Int) -> Bool { { rolling.contains($0) && ($0 - rolling.lowerBound) % 2 == p } }
        guard let plain = image(c, glow: false, pad: pad, shift: shift, drawn: letters > 0 ? { !rolling.contains($0) } : nil) else { return }
        let letterImages = letters > 0 ? [0, 1].compactMap { image(c, glow: false, pad: pad, shift: shift, drawn: parity($0)) } : []
        let glowImages = letters > 0 && c.hold != nil ? [0, 1].compactMap { image(c, glow: true, pad: pad, shift: shift, drawn: parity($0)) } : []
        let size = CGSize(width: CGFloat(plain.width) / scale, height: CGFloat(plain.height) / scale)
        clock.frame = CGRect(origin: CGPoint(x: aligned.x - pad, y: aligned.y - pad), size: size)
        let offset = CGPoint(x: pad + shift.x, y: pad + shift.y)
        func snap(_ value: CGFloat) -> CGFloat { (value * scale).rounded() / scale }

        let timing = LyricTiming(start: c.start, span: c.span, line: c.line, hold: c.hold, wave: c.wave, whole: c.whole)
        let words = c.lift > 0 ? timing.words(in: c.text) : []
        func liftSpan(_ word: LyricWordSpan) -> (from: Double, to: Double) {
            let from = word.startMs.map { $0 - T.lyricWordLiftLead * 1000 } ?? Double(c.start)
            return (from, word.startMs == nil ? Double(c.start + c.span) : from + T.lyricWordLiftTime * 1000)
        }
        let lifts: [CAKeyframeAnimation?] = words.map { word in
            let (from, to) = liftSpan(word)
            let values = Self.times(from: from, to: to).map { -c.lift * CGFloat(timing.rise(of: word, count: count, at: $0) ?? 0) }
            return values.contains { $0 != 0 } ? Self.animation("transform.translation.y", values, from: from, to: to) : nil
        }
        // each letter rises with its word and rolls on top.
        func roll(_ k: Int, of word: LyricWordSpan?) -> CAKeyframeAnimation {
            let wave = c.wave ?? 0..<0
            let lift = word.map(liftSpan) ?? (from: Double(wave.lowerBound), to: Double(wave.lowerBound))
            let from = min(lift.from, Double(wave.lowerBound)), to = max(lift.to, Double(wave.upperBound))
            let values = Self.times(from: from, to: to).map { ms in
                -c.lift * CGFloat(word.flatMap { timing.rise(of: $0, count: count, at: ms) } ?? 0)
                    - c.size * CGFloat(LyricWave.rise(letter: k, at: (ms - Double(wave.lowerBound)) / 1000, held: Double(wave.count) / 1000))
            }
            return Self.animation("transform.translation.y", values, from: from, to: to)
        }
        var letterFrames: [(letter: Int, frame: CGRect, roll: CAKeyframeAnimation)] = []

        let base = CALayer()
        base.frame = CGRect(origin: .zero, size: size)
        base.opacity = Float(T.lyricUnsung.a)
        clock.addSublayer(base)

        let total = c.lines.reduce(0) { $0 + $1.count }
        let fillEnd = max(Double(c.start + c.span), c.line?.words.last.map { Double($0.endMs ?? $0.startMs) } ?? 0)
        let fillTimes = Self.times(from: Double(c.start), to: fillEnd)
        let progress = fillTimes.map { timing.progress(at: $0) }
        var lastWordLift: CAKeyframeAnimation?
        var lit: [CALayer] = []

        for (row, line) in c.lines.enumerated() {
            let top = row == 0 ? 0 : snap((c.lines[row - 1].bounds.maxY + line.bounds.minY) / 2 + offset.y)
            let bottom = row == c.lines.count - 1 ? size.height : snap((line.bounds.maxY + c.lines[row + 1].bounds.minY) / 2 + offset.y)

            // one piece per word, split in the middle of the spaces, so each word can rise by itself.
            var pieces: [(word: Int, box: CGRect)] = []
            for (index, word) in words.enumerated() {
                let glyphs = word.chars.clamped(to: line.start..<line.start + line.count).filter { !rolling.contains($0) }
                guard !glyphs.isEmpty else { continue }
                pieces.append((index, glyphs.map { line.glyphs[$0 - line.start] }.reduce(CGRect.null) { $0.union($1) }))
            }
            pieces.sort { $0.box.minX < $1.box.minX }
            var cuts: [CGFloat] = [0]
            for (a, b) in zip(pieces, pieces.dropFirst()) { cuts.append(snap((a.box.maxX + b.box.minX) / 2 + offset.x)) }
            cuts.append(size.width)

            let sung = CALayer()
            sung.frame = CGRect(origin: .zero, size: size)
            for index in 0..<cuts.count - 1 {
                let frame = CGRect(x: cuts[index], y: top, width: cuts[index + 1] - cuts[index], height: bottom - top)
                guard frame.width > 0, frame.height > 0,
                      let piece = plain.cropping(to: CGRect(x: frame.minX * scale, y: frame.minY * scale, width: frame.width * scale, height: frame.height * scale).integral) else { continue }
                let lift = pieces.indices.contains(index) ? lifts[pieces[index].word] : nil
                for parent in [base, sung] {
                    let layer = CALayer()
                    layer.frame = frame
                    layer.contents = piece
                    if let lift { layer.add(lift, forKey: "lift") }
                    parent.addSublayer(layer)
                }
                if let lift, pieces[index].word == words.count - 1 { lastWordLift = lift }
            }

            // each rolling letter on this row, from its own image, between the middles of the letters beside it.
            let mine = Array(rolling.clamped(to: line.start..<line.start + line.count)).sorted { line.glyphs[$0 - line.start].minX < line.glyphs[$1 - line.start].minX }
            for (j, glyph) in mine.enumerated() where letterImages.count == 2 {
                let box = line.glyphs[glyph - line.start], k = glyph - rolling.lowerBound
                let left = max(0, snap((j == 0 ? box.minX - pad : line.glyphs[mine[j - 1] - line.start].midX) + offset.x))
                let right = min(size.width, snap((j == mine.count - 1 ? box.maxX + pad : line.glyphs[mine[j + 1] - line.start].midX) + offset.x))
                let frame = CGRect(x: left, y: top, width: right - left, height: bottom - top)
                guard frame.width > 0, frame.height > 0,
                      let piece = letterImages[k % 2].cropping(to: CGRect(x: frame.minX * scale, y: frame.minY * scale, width: frame.width * scale, height: frame.height * scale).integral) else { continue }
                let rolled = roll(k, of: words.first { $0.chars.contains(glyph) })
                letterFrames.append((k, frame, rolled))
                for parent in [base, sung] {
                    let layer = CALayer()
                    layer.frame = frame
                    layer.contents = piece
                    layer.add(rolled, forKey: "lift")
                    parent.addSublayer(layer)
                }
            }

            let soft = line.edge(reached: 0, timed: c.line != nil).soft
            // before the fill reaches the line, the edge waits clear of it.
            let hidden = line.rtl ? line.band.maxX + soft + 1 : line.band.minX - soft - 1
            let before = c.lines[..<row].reduce(0) { $0 + $1.count }
            let edges: [CGFloat] = progress.map { progress in
                let reached = max(0, min(Double(line.count), progress * Double(max(1, total)) - Double(before)))
                return (reached > 0 ? line.edge(reached: reached, timed: c.line != nil).edge : hidden) + offset.x
            }
            let mask = CALayer()
            mask.frame = CGRect(origin: .zero, size: size)
            let mover = CALayer()
            let far = size.width + line.bounds.width * 4
            let full = CALayer()
            full.backgroundColor = NSColor.black.cgColor
            full.frame = CGRect(x: line.rtl ? 0 : -far, y: top, width: far, height: bottom - top)
            let fade = CAGradientLayer()
            fade.colors = line.rtl ? [NSColor.clear.cgColor, NSColor.black.cgColor] : [NSColor.black.cgColor, NSColor.clear.cgColor]
            fade.startPoint = CGPoint(x: 0, y: 0.5)
            fade.endPoint = CGPoint(x: 1, y: 0.5)
            fade.frame = CGRect(x: line.rtl ? -soft : 0, y: top, width: soft, height: bottom - top)
            mover.addSublayer(full)
            mover.addSublayer(fade)
            mover.position = CGPoint(x: hidden + offset.x, y: 0)
            mover.add(Self.animation("position.x", edges, from: Double(c.start), to: fillEnd), forKey: "fill")
            mask.addSublayer(mover)
            sung.mask = mask
            lit.append(sung)
        }

        // under the sung text and over the unsung, rising with its word.
        if let hold = c.hold, c.lastWord > 0 {
            let from = Double(hold.lowerBound), to = Double(hold.upperBound)
            let strength = Self.animation("opacity", Self.times(from: from, to: to).map { CGFloat(timing.holdStrength(at: $0)) }, from: from, to: to)
            // rolling, each letter's halo comes from its own image and rides its letter.
            let halos: [(image: CGImage?, frame: CGRect, lift: CAKeyframeAnimation?)] = glowImages.count == 2 && !letterFrames.isEmpty
                ? letterFrames.map { (glowImages[$0.letter % 2], CGRect(x: $0.frame.minX, y: 0, width: $0.frame.width, height: size.height), $0.roll) }
                : [(image(c, glow: true, pad: pad, shift: shift), CGRect(origin: .zero, size: size), lastWordLift)]
            for halo in halos {
                guard let piece = halo.image?.cropping(to: CGRect(x: halo.frame.minX * scale, y: 0, width: halo.frame.width * scale, height: size.height * scale).integral) else { continue }
                let glow = CALayer()
                glow.frame = halo.frame
                glow.contents = piece
                glow.opacity = 0
                glow.add(strength, forKey: "hold")
                if let lift = halo.lift { glow.add(lift, forKey: "lift") }
                clock.addSublayer(glow)
            }
        }
        for layer in lit { clock.addSublayer(layer) }
    }
}

final class LyricGapView: LyricLayerView {
    struct Content: Equatable {
        var size: CGFloat
        var start: Int
        var span: Int
    }

    private var content: Content?
    private var built: (content: Content, size: CGSize)?

    func show(_ content: Content) {
        guard content != self.content else { return }
        // another gap's bars go at once (see `LyricFillView.show`).
        if content.start != self.content?.start {
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            clock.sublayers = nil
            CATransaction.commit()
            built = nil
        }
        self.content = content
        needsLayout = true
    }

    override func layout() {
        super.layout()
        guard window != nil, let content, built.map({ $0.content != content || $0.size != bounds.size }) ?? true else { return }
        built = (content, bounds.size)
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        build(content)
        CATransaction.commit()
        sync()
    }

    private func build(_ c: Content) {
        clock.sublayers = nil
        clock.frame = bounds
        let count = T.lyricGapBars, bar = c.size * T.lyricGapWidth, quiet = c.size * T.lyricGapBarMin, full = c.size * T.lyricGapHeight
        let gap = c.size * T.lyricGapBarGap
        let from = Double(c.start), to = Double(c.start + c.span)
        let states = Self.times(from: from, to: to).map {
            LyricGapState(progress: max(0, min(1, ($0 - from) / max(1, to - from))), start: from / 1000, seconds: (to - from) / 1000)
        }
        func animate(_ layer: CALayer, _ keyPath: String, _ value: (LyricGapState) -> Double) {
            layer.add(Self.animation(keyPath, states.map { CGFloat(value($0)) }, from: from, to: to), forKey: keyPath)
        }

        // outermost first, as `GapMarker` stacks them.
        let whole = CALayer(), swell = CALayer(), rest = CALayer()
        for layer in [whole, swell, rest] { layer.frame = bounds }
        animate(whole, "transform.scale", \.scale)
        animate(whole, "opacity", \.opacity)
        swell.shadowColor = NSColor(T.tOnArt.color).cgColor
        swell.shadowOffset = .zero
        animate(swell, "shadowOpacity") { T.lyricGapSwellGlow * $0.swell }
        animate(swell, "shadowRadius") { bar * T.lyricGapGlow * (1 + $0.swell) }
        let glow = T.lyricGlow[T.lyricGlow.count - 1].color
        rest.shadowColor = NSColor(glow.color).cgColor
        rest.shadowOffset = .zero
        rest.shadowOpacity = 1
        rest.shadowRadius = bar * T.lyricGapGlow
        clock.addSublayer(whole)
        whole.addSublayer(swell)
        swell.addSublayer(rest)

        let left = (bounds.width - (CGFloat(count) * bar + CGFloat(count - 1) * gap)) / 2
        let scale = window?.backingScaleFactor ?? 1
        // bar sides sit on whole pixels, as SwiftUI lays the same bars out.
        func snap(_ value: CGFloat) -> CGFloat { (value * scale).rounded() / scale }
        for index in 0..<count {
            let from = snap(left + CGFloat(index) * (bar + gap)), to = snap(left + CGFloat(index) * (bar + gap) + bar)
            for lit in [false, true] {
                let layer = CALayer()
                layer.backgroundColor = NSColor((lit ? T.tOnArt : T.lyricGapUnlit).color).cgColor
                layer.cornerRadius = (to - from) / 2
                layer.bounds = CGRect(x: 0, y: 0, width: to - from, height: quiet)
                layer.position = CGPoint(x: (from + to) / 2, y: bounds.midY)
                animate(layer, "bounds.size.height") { quiet + $0.level(index) * (full - quiet) }
                if lit { animate(layer, "opacity") { $0.light(index) } }
                rest.addSublayer(layer)
            }
        }
    }
}
