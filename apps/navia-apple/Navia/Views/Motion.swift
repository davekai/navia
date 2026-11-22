import AppKit
import SwiftUI

/// how far the sidebar, side player and bar are in, 0 to 1.
struct PanelShown: Equatable {
    var sidebar: CGFloat
    var dock: CGFloat
    var bar: CGFloat

    static let none = PanelShown(sidebar: 0, dock: 0, bar: 0)

    /// these flags with `coming`'s added, cleared for each panel `target` leaves out.
    func keeping(_ coming: PanelShown, in target: PanelShown) -> PanelShown {
        PanelShown(sidebar: target.sidebar > 0 ? max(sidebar, coming.sidebar) : 0, dock: target.dock > 0 ? max(dock, coming.dock) : 0,
                   bar: target.bar > 0 ? max(bar, coming.bar) : 0)
    }
}

extension View {
    /// a panel waiting to slide in sits in place nearly transparent, so its first draw happens before the slide; at 0 it wouldn't be drawn at all.
    func panelWarming(_ warming: CGFloat) -> some View {
        opacity(warming > 0 ? T.panelWarmOpacity : 1).allowsHitTesting(warming == 0)
    }
}

/// lays out its content for panels that are partly in: SwiftUI interpolates `shown` under an animation (`T.panelSlide`)
/// and builds the content at each step, so the page resizes with the slide and a change mid-way turns back from where it is.
struct PanelSlide<Content: View>: View, Animatable {
    var shown: PanelShown
    @ViewBuilder let content: (PanelShown) -> Content

    var animatableData: AnimatablePair<CGFloat, AnimatablePair<CGFloat, CGFloat>> {
        get { AnimatablePair(shown.sidebar, AnimatablePair(shown.dock, shown.bar)) }
        set { shown = PanelShown(sidebar: newValue.first, dock: newValue.second.first, bar: newValue.second.second) }
    }

    var body: some View {
        // each step is already in place; the content's own frames mustn't animate toward it again.
        content(shown).transaction(value: shown) { $0.animation = nil }
    }
}

/// slides `PanelSlide` toward `target` over `T.panelSlide`. a panel coming in from 0 is first built and drawn in place
/// but unseen (`staged` and `warming` 1, see `panelWarming`), and slides once the app keeps up with the display again,
/// since its first build and first draw take longer than a frame and would eat the start of the slide.
struct PanelStage<Content: View>: View {
    let target: PanelShown
    /// false while the window isn't shown: everything lands at once.
    let live: Bool
    @ViewBuilder let content: (_ shown: PanelShown, _ staged: PanelShown, _ warming: PanelShown) -> Content
    @State private var shown: PanelShown
    @State private var staged = PanelShown.none
    @State private var warming = PanelShown.none
    @State private var drawn = Drawn()
    @State private var ticket = 0

    init(target: PanelShown, live: Bool, @ViewBuilder content: @escaping (_ shown: PanelShown, _ staged: PanelShown, _ warming: PanelShown) -> Content) {
        self.target = target
        self.live = live
        self.content = content
        _shown = State(initialValue: target)
    }

    var body: some View {
        PanelSlide(shown: shown) { step in
            let _ = drawn.shown = step
            content(step, staged, warming)
        }
        .onChange(of: target) { _, new in move(to: new) }
    }

    private func move(to new: PanelShown) {
        ticket += 1
        let animation = live ? T.ease(T.panelSlide) : nil
        // only a panel that's fully out now; one still sliding turns back from where it is.
        let now = drawn.shown
        let coming = PanelShown(sidebar: now.sidebar == 0 && new.sidebar > 0 ? 1 : 0,
                                dock: now.dock == 0 && new.dock > 0 ? 1 : 0,
                                bar: now.bar == 0 && new.bar > 0 ? 1 : 0)
        var settle = Transaction(animation: nil)
        settle.disablesAnimations = true
        let slides = animation != nil && coming != .none
        withTransaction(settle) {
            staged = staged.keeping(coming, in: new)
            warming = slides ? warming.keeping(coming, in: new) : .none
        }
        guard slides else {
            withAnimation(animation) { shown = new }
            return
        }
        let mine = ticket
        SteadyFrame.wait {
            guard mine == ticket else { return }
            withTransaction(settle) { warming = .none }
            withAnimation(animation) { shown = new }
        }
    }
}

/// the step last drawn, as the slide's position isn't readable from its state.
private final class Drawn {
    var shown = PanelShown.none
}

/// runs `then` once a display frame has come on time again, so an animation started there has its first frame drawn
/// at its start; at most `T.panelBuildWait` later.
@MainActor private final class SteadyFrame: NSObject {
    private let then: () -> Void
    private let since = CACurrentMediaTime()
    private var last: CFTimeInterval?
    private var link: CADisplayLink?

    private init(_ then: @escaping () -> Void) { self.then = then }

    static func wait(_ then: @escaping () -> Void) {
        guard let screen = NSScreen.main else { DispatchQueue.main.async(execute: then); return }
        let waiter = SteadyFrame(then)
        // the link keeps its target alive until it's invalidated.
        let link = screen.displayLink(target: waiter, selector: #selector(tick(_:)))
        waiter.link = link
        link.add(to: .main, forMode: .common)
    }

    @objc private func tick(_ link: CADisplayLink) {
        let now = CACurrentMediaTime()
        // one frame after the last, give or take half: not late, and not a catch-up tick after a late one.
        let frame = link.targetTimestamp - link.timestamp
        let onTime = last.map { now - $0 > frame / 2 && now - $0 < frame * 1.5 } ?? false
        last = now
        guard onTime || now - since >= T.panelBuildWait else { return }
        link.invalidate()
        self.link = nil
        then()
    }
}

/// what a `Swap` cross-fades: the player's song (artwork and title) or its view (artwork, lyrics, queue), which rises as it comes in.
enum SwapKind {
    case song, view

    var duration: Double { self == .song ? T.songFade : T.viewFade }
    var rise: CGFloat { self == .song ? 0 : T.viewRise }
}

/// cross-fades its content when `key` changes: the old one fades out over the new one, which fades in.
struct Swap<Key: Hashable, Content: View>: View {
    let key: Key
    let kind: SwapKind
    var alignment: Alignment = .top
    @ViewBuilder let content: () -> Content
    @Environment(\.motionLive) private var live

    var body: some View {
        ZStack(alignment: alignment) {
            content()
                .id(key)
                .transition(.asymmetric(insertion: .opacity.combined(with: .offset(y: kind.rise)), removal: .opacity))
        }
        .animation(live ? T.ease(kind.duration) : nil, value: key)
    }
}

/// A new page fading in over `T.pageFade`, rising `T.pageRise` (`entry` 1), or falling after Back (-1); nil for the first page.
struct PageEntrance: ViewModifier {
    let entry: CGFloat?
    @State private var entered: Bool
    @Environment(\.motionLive) private var live

    init(entry: CGFloat?) {
        self.entry = entry
        _entered = State(initialValue: entry == nil || Motion.isReduced)
    }

    func body(content: Content) -> some View {
        content
            .opacity(entered ? 1 : 0)
            .offset(y: entered ? 0 : T.pageRise * (entry ?? 0))
            .onAppear { if !entered { withAnimation(live ? T.ease(T.pageFade) : nil) { entered = true } } }
    }
}

/// the heart's pop as a song is liked: up to `T.likePopScale` at `T.likePopPeak` of `T.likePop` and back, the whole
/// timeline on `--ease`.
enum LikePop {
    static let ease = UnitCurve.bezier(startControlPoint: UnitPoint(x: 0.2, y: 0.7), endControlPoint: UnitPoint(x: 0.2, y: 1))

    static func scale(_ progress: Double) -> CGFloat {
        let e = ease.value(at: min(max(progress, 0), 1))
        let k = e < T.likePopPeak ? e / T.likePopPeak : (1 - e) / (1 - T.likePopPeak)
        return 1 + (T.likePopScale - 1) * k
    }
}

/// pops its content each time `trigger` changes (see `LikePop`).
struct LikePopEffect: ViewModifier {
    let trigger: Int

    func body(content: Content) -> some View {
        content.keyframeAnimator(initialValue: 1.0, trigger: trigger) { view, progress in
            view.scaleEffect(LikePop.scale(progress))
        } keyframes: { _ in
            MoveKeyframe(0.0)
            LinearKeyframe(1.0, duration: T.likePop)
        }
    }
}

extension NSView {
    /// `LikePop` for an AppKit control, about its centre.
    func likePop() {
        guard !Motion.isReduced else { return }
        wantsLayer = true
        guard let layer else { return }
        let steps = 12
        let c = CGPoint(x: bounds.midX, y: bounds.midY)
        let animation = CAKeyframeAnimation(keyPath: "transform")
        animation.values = (0...steps).map { i in
            let s = LikePop.scale(Double(i) / Double(steps))
            return NSValue(caTransform3D: CATransform3DConcat(CATransform3DConcat(CATransform3DMakeTranslation(-c.x, -c.y, 0), CATransform3DMakeScale(s, s, 1)), CATransform3DMakeTranslation(c.x, c.y, 0)))
        }
        animation.duration = T.likePop
        layer.add(animation, forKey: "likePop")
    }
}

/// songs just queued or added to a playlist: their rows light with `--row-flash-fill` and fade back over `T.rowFlash`.
@Observable final class RowFlash {
    static let shared = RowFlash()
    static let didFlash = Notification.Name("naviaRowFlash")
    private(set) var songs: Set<String> = []
    /// counts flashes, so a second one on the same songs lights them again.
    private(set) var count = 0
    @ObservationIgnored private(set) var since = Date.distantPast

    func flash(_ ids: some Sequence<String>) {
        songs = Set(ids)
        since = Date()
        count += 1
        NotificationCenter.default.post(name: Self.didFlash, object: nil)
    }
}

/// A row's flash, behind its content; under reduced motion it holds, then drops.
struct RowFlashFill: View {
    let id: String
    let radius: CGFloat
    @Environment(\.theme) private var theme
    @Environment(\.motionLive) private var live
    @State private var lit = 0.0
    @State private var drop: Task<Void, Never>?

    var body: some View {
        let flash = RowFlash.shared
        RoundedRectangle(cornerRadius: radius, style: .continuous)
            .fill((theme?.rowFlashFill ?? Theme.rowFlashFill(Theme.defaultAccent)).color)
            .opacity(lit)
            .allowsHitTesting(false)
            .onChange(of: flash.count) { light(flash) }
            // A row the change itself added (in the queue) lights as it appears.
            .onAppear { if Date().timeIntervalSince(flash.since) < T.rowFlash { light(flash) } }
    }

    private func light(_ flash: RowFlash) {
        guard flash.songs.contains(id), live else { return }
        drop?.cancel()
        lit = 1
        if Motion.isReduced {
            drop = Task { try? await Task.sleep(for: .seconds(T.rowFlash)); if !Task.isCancelled { lit = 0 } }
        } else {
            withAnimation(.easeOut(duration: T.rowFlash)) { lit = 0 }
        }
    }
}

extension CALayer {
    /// `RowFlashFill` for an AppKit row: lights `rect` and fades it back, or holds then drops under reduced motion.
    func rowFlash(_ fill: RGBA, rect: CGRect, radius: CGFloat) {
        let layer = sublayers?.first { $0.name == "rowFlash" } ?? {
            let new = CALayer()
            new.name = "rowFlash"
            new.opacity = 0
            insertSublayer(new, at: 0)
            return new
        }()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        layer.frame = rect
        layer.cornerRadius = radius
        layer.backgroundColor = fill.ns.cgColor
        CATransaction.commit()
        let animation = CAKeyframeAnimation(keyPath: "opacity")
        animation.values = [1, 0]
        animation.duration = T.rowFlash
        if Motion.isReduced {
            animation.calculationMode = .discrete
        } else {
            animation.timingFunction = CAMediaTimingFunction(name: .easeOut)
        }
        layer.add(animation, forKey: "rowFlash")
    }
}

/// A page section that arrives after the page's entrance (`T.pageFade`) fades in over `T.landFade`, each one
/// `T.landStagger` after the one before; those past `T.landStaggerMax` share its delay.
struct LandSection: ViewModifier {
    let index: Int
    @State private var shown: Bool
    @Environment(\.motionLive) private var live

    init(index: Int, pageSince: Date) {
        self.index = index
        _shown = State(initialValue: Motion.isReduced || Date().timeIntervalSince(pageSince) < T.pageFade)
    }

    func body(content: Content) -> some View {
        content
            .opacity(shown ? 1 : 0)
            .onAppear {
                guard !shown else { return }
                withAnimation(live ? T.ease(T.landFade)?.delay(Double(min(index, T.landStaggerMax)) * T.landStagger) : nil) { shown = true }
            }
    }
}
