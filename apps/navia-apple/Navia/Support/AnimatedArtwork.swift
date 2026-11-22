import AppKit
import NaviaCoreFFI
import SwiftUI
import Synchronization

/// decoded once per display size; they loop forever with `delays`.
nonisolated final class AnimatedFrames: @unchecked Sendable {
    let images: [CGImage]
    /// seconds per frame.
    let delays: [Double]
    var duration: Double { delays.reduce(0, +) }
    var aspect: CGFloat { images.first.map { CGFloat($0.width) / CGFloat(max(1, $0.height)) } ?? 1 }

    /// call off the main thread.
    init?(_ art: NaviaAnimatedArt) {
        let width = Int(art.width()), height = Int(art.height())
        guard width > 0, height > 0, let raw = try? art.frames(), raw.count > 1 else { return nil }
        let space = CGColorSpace(name: CGColorSpace.sRGB)!
        // the core's frames are straight (not premultiplied) RGBA.
        let info = CGBitmapInfo(rawValue: (art.opaque() ? CGImageAlphaInfo.noneSkipLast : CGImageAlphaInfo.last).rawValue)
        let images = raw.compactMap { data -> CGImage? in
            guard data.count == width * height * 4, let provider = CGDataProvider(data: data as CFData) else { return nil }
            return CGImage(width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: width * 4, space: space,
                           bitmapInfo: info, provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent)
        }
        let delays = art.delaysMs().map { Double($0) / 1000 }
        guard images.count == raw.count, delays.count == images.count, delays.reduce(0, +) > 0 else { return nil }
        self.images = images
        self.delays = delays
        #if DEBUG
        AnimatedArtProbe.live.add(1, ordering: .relaxed)
        #endif
    }

    #if DEBUG
    deinit { AnimatedArtProbe.live.subtract(1, ordering: .relaxed) }
    #endif
}

#if DEBUG
enum AnimatedArtProbe {
    nonisolated static let live = Atomic<Int>(0)
    @MainActor static let views = NSHashTable<AnimatedArtNSView>.weakObjects()

    @MainActor static func report() -> [String: Any] {
        ["liveFrameSets": live.load(ordering: .relaxed),
         "views": views.allObjects.map { $0.probe }]
    }
}
#endif

/// on Core Animation, so SwiftUI isn't redrawn per frame; rests on the first frame while unseen or under Reduce Motion.
struct AnimatedArtLayer: NSViewRepresentable {
    let frames: AnimatedFrames
    var alignment: Alignment = .center

    func makeNSView(context: Context) -> AnimatedArtNSView { AnimatedArtNSView(frames: frames, alignment: alignment) }

    func updateNSView(_ view: AnimatedArtNSView, context: Context) {
        view.alignment = alignment
        if view.frames !== frames { view.frames = frames }
    }
}

final class AnimatedArtNSView: NSView {
    var frames: AnimatedFrames { didSet { restart() } }
    var alignment: Alignment { didSet { needsLayout = true } }
    private let art = CALayer()
    private var observers: [NSObjectProtocol] = []
    private var playing = false
    private static let key = "frames"

    init(frames: AnimatedFrames, alignment: Alignment) {
        self.frames = frames
        self.alignment = alignment
        super.init(frame: .zero)
        wantsLayer = true
        layer?.masksToBounds = true
        art.contentsGravity = .resize
        art.actions = ["contents": NSNull(), "bounds": NSNull(), "position": NSNull()]
        layer?.addSublayer(art)
        art.contents = frames.images.first
        setAccessibilityElement(false)
        #if DEBUG
        AnimatedArtProbe.views.add(self)
        #endif
    }

    #if DEBUG
    var probe: [String: Any] {
        ["running": art.animation(forKey: Self.key) != nil, "speed": art.speed, "frames": frames.images.count,
         "inWindow": window != nil, "occludedVisible": window?.occlusionState.contains(.visible) ?? false,
         "miniaturized": window?.isMiniaturized ?? false, "visibleRectEmpty": visibleRect.isEmpty,
         "size": [Int(bounds.width), Int(bounds.height)]]
    }
    #endif

    required init?(coder: NSCoder) { fatalError("not used") }

    override var isFlipped: Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func layout() {
        super.layout()
        // fill like `object-fit: cover`, placed by the alignment.
        let box = bounds.size
        guard box.width > 0, box.height > 0 else { return }
        let aspect = frames.aspect
        var size = CGSize(width: box.width, height: box.width / aspect)
        if size.height < box.height { size = CGSize(width: box.height * aspect, height: box.height) }
        let x = (box.width - size.width) / 2
        let y: CGFloat = switch alignment.vertical {
        case .top: 0
        case .bottom: box.height - size.height
        default: (box.height - size.height) / 2
        }
        art.frame = CGRect(origin: CGPoint(x: x, y: y), size: size)
        art.contentsScale = window?.backingScaleFactor ?? 2
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        observers.forEach { NotificationCenter.default.removeObserver($0) }
        observers.removeAll()
        NSWorkspace.shared.notificationCenter.removeObserver(self)
        guard let window else { stop(); return }
        let center = NotificationCenter.default
        for name in [NSWindow.didChangeOcclusionStateNotification, NSWindow.didMiniaturizeNotification, NSWindow.didDeminiaturizeNotification] {
            observers.append(center.addObserver(forName: name, object: window, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.update() }
            })
        }
        if let clip = enclosingScrollView?.contentView {
            clip.postsBoundsChangedNotifications = true
            observers.append(center.addObserver(forName: NSView.boundsDidChangeNotification, object: clip, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.update() }
            })
        }
        NSWorkspace.shared.notificationCenter.addObserver(self, selector: #selector(motionChanged),
                                                          name: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification, object: nil)
        update()
    }

    @objc private func motionChanged() { update() }

    private var shouldPlay: Bool {
        guard let window, !Motion.isReduced, !window.isMiniaturized, window.occlusionState.contains(.visible) else { return false }
        return !visibleRect.isEmpty
    }

    private func update() {
        if shouldPlay { play() } else { stop() }
    }

    private func restart() {
        stop()
        art.contents = frames.images.first
        needsLayout = true
        update()
    }

    private func play() {
        guard !playing else { return }
        playing = true
        let total = frames.duration
        var times: [NSNumber] = [0]
        var at = 0.0
        for delay in frames.delays.dropLast() {
            at += delay
            times.append(NSNumber(value: at / total))
        }
        let animation = CAKeyframeAnimation(keyPath: "contents")
        animation.values = frames.images
        animation.keyTimes = times
        animation.calculationMode = .discrete
        animation.duration = total
        animation.repeatCount = .infinity
        animation.isRemovedOnCompletion = false
        art.add(animation, forKey: Self.key)
    }

    private func stop() {
        guard playing else { return }
        playing = false
        art.removeAnimation(forKey: Self.key)
        art.contents = frames.images.first
    }
}
