import AppKit
import QuartzCore

/// the album table's Heat column: Deezer's `rank` as five thin bars against the
/// table's own range, the hottest song full and tinted, with a light passing across it while hovered or playing.
struct HeatScale {
    static let bars = 5
    /// Deezer's `rank` tops out near a million.
    private static let rankMax = 1_000_000.0
    /// the table's least popular song keeps a bar, so it doesn't read as unknown.
    private static let floor = 0.15

    private let ranks: [String: UInt64]
    private let low: UInt64
    private let top: UInt64

    init(ranks: [String: UInt64] = [:]) {
        self.ranks = ranks
        low = ranks.values.min() ?? 0
        top = ranks.values.max() ?? 0
    }

    var isEmpty: Bool { ranks.isEmpty }

    /// bars lit for a song. all five are kept for the table's most popular songs, so a full meter always means the hottest.
    func lit(_ song: String) -> Int? {
        guard let rank = ranks[song] else { return nil }
        if rank >= top { return Self.bars }
        // Deezer's ranks bunch near the top on a popular album, so the table's own range is stretched over the meter.
        let share = top > low ? Self.floor + (1 - Self.floor) * Double(rank - low) / Double(top - low) : 1
        return min(Self.bars - 1, max(1, Int((share * Double(Self.bars)).rounded(.up))))
    }

    /// "#2 of 10 on this album · Very popular on Deezer".
    func tip(_ song: String) -> String? {
        guard let rank = ranks[song] else { return nil }
        let place = 1 + ranks.values.filter { $0 > rank }.count
        return L("#{place} of {count} on this album · {level}", ["place": "\(place)", "count": "\(ranks.count)", "level": Self.level(rank)])
    }

    private static func level(_ rank: UInt64) -> String {
        switch Int((Double(rank) / rankMax * Double(bars)).rounded(.up)) {
        case 5...: L("Very popular on Deezer")
        case 4: L("Popular on Deezer")
        case 3: L("Fairly popular on Deezer")
        case 2: L("Less popular on Deezer")
        default: L("Little known on Deezer")
        }
    }
}

/// every bar on whole device pixels, so the thin bars stay crisp and even at any scale. the sweep is a
/// Core Animation keyframe animation, so the app doesn't redraw while the light moves.
final class HeatMeterView: NSView {
    private var bars: [CAGradientLayer] = []
    private var lights: [CALayer] = []
    private var lit = 0
    private var hot: RGBA?
    private var now = false
    private var sweeping = false

    override var isFlipped: Bool { true }

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        setAccessibilityElement(true)
        setAccessibilityRole(.image)
        for _ in 0..<HeatScale.bars {
            let bar = CAGradientLayer()
            let light = CALayer()
            light.backgroundColor = T.popularityHeatTop.ns.cgColor
            light.opacity = 0
            layer?.addSublayer(bar)
            layer?.addSublayer(light)
            bars.append(bar)
            lights.append(light)
        }
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    /// `hot` is the cover's tint for the hottest song, whose bars run from it to white.
    func configure(lit: Int, hot: RGBA?, now: Bool, tip: String, accentSoft: RGBA) {
        self.lit = lit
        self.hot = hot
        self.now = now
        toolTip = tip
        setAccessibilityLabel(tip)
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        for (i, bar) in bars.enumerated() {
            if let hot {
                bar.colors = [hot.ns.cgColor, T.popularityHeatTop.ns.cgColor]
            } else {
                let color = i < lit ? (now ? accentSoft : T.popularityOn) : T.popularityOff
                bar.colors = [color.ns.cgColor, color.ns.cgColor]
            }
            // bottom to top.
            bar.startPoint = CGPoint(x: 0.5, y: 1)
            bar.endPoint = CGPoint(x: 0.5, y: 0)
        }
        CATransaction.commit()
        needsLayout = true
    }

    /// the light passes on the hottest song while its row is hovered or playing, and never under Reduce Motion.
    func setSweeping(_ on: Bool) {
        let on = on && hot != nil && !Motion.isReduced
        guard on != sweeping else { return }
        sweeping = on
        let start = CACurrentMediaTime()
        for (i, light) in lights.enumerated() {
            light.removeAllAnimations()
            guard on else { continue }
            let flash = CAKeyframeAnimation(keyPath: "opacity")
            flash.values = [0, 1, 0, 0]
            flash.keyTimes = [0, NSNumber(value: T.heatFlashPeak), NSNumber(value: T.heatFlashEnd), 1]
            let ease = CAMediaTimingFunction(controlPoints: Float(T.ease.0), Float(T.ease.1), Float(T.ease.2), Float(T.ease.3))
            flash.timingFunctions = [ease, ease, CAMediaTimingFunction(name: .linear)]
            flash.duration = T.popularityHeatSweep
            flash.repeatCount = .infinity
            flash.beginTime = layer.map { $0.convertTime(start, from: nil) } ?? start
            flash.beginTime += Double(i) * T.popularityHeatSweepStep
            light.add(flash, forKey: "sweep")
        }
    }

    override func layout() {
        super.layout()
        // sizes in whole device pixels, then every bar from one snapped origin, so bars and gaps all match.
        let scale = window?.backingScaleFactor ?? 2
        let device = { (v: CGFloat) in max(1, (v * scale).rounded()) / scale }
        let w = device(T.popularityBarW), gap = device(T.popularityBarGap), h = device(T.popularityBarH)
        let top = convert(NSPoint(x: 0, y: (bounds.height - h) / 2), to: nil)
        let origin = convert(NSPoint(x: (top.x * scale).rounded() / scale, y: (top.y * scale).rounded() / scale), from: nil)
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        for i in 0..<HeatScale.bars {
            let rect = NSRect(x: origin.x + CGFloat(i) * (w + gap), y: origin.y, width: w, height: h)
            for layer in [bars[i], lights[i]] {
                layer.frame = rect
                layer.cornerRadius = w / 2
                layer.contentsScale = scale
            }
        }
        CATransaction.commit()
    }

    override func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        needsLayout = true
    }

    /// the meter's own width.
    static var width: CGFloat { CGFloat(HeatScale.bars) * (T.popularityBarW + T.popularityBarGap) - T.popularityBarGap }
}
