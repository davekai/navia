import QuartzCore
import SwiftUI

/// Where a page's content sits in the window. The page spans the window so shelves and tints reach under the
/// sidebar and dock.
struct PageContext {
    var sidebarUnder: CGFloat = 0
    /// How far horizontal rows extend under the glass sidebar.
    var shelfUnder: CGFloat = 0
    var dockUnder: CGFloat = 0
    /// how far the side player is in, 0 to 1, as it slides.
    var dockShown: CGFloat = 0
    var hasBar = false
    var sidebarOpen = true
    var pageWidth: CGFloat = 0
    /// The page width ignoring a dock drag in progress. Grids reflow from this, so they settle when the drag ends.
    var layoutWidth: CGFloat = 0
    var windowHeight: CGFloat = 0
    /// when the page was opened, so sections arriving after its entrance fade in (`LandSection`).
    var pageSince = Date.distantPast
    /// The page's side margin: `T.gutter`, or `T.splitGutter` for the page beside the split list.
    var gutter: CGFloat = T.gutter
    var end: CGFloat = 0
    var toggleSidebar: () -> Void = {}

    var cssWidth: CGFloat { pageWidth / T.scale }
    var tableLeading: CGFloat { sidebarUnder + gutter - T.gutter }
    var tableTrailing: CGFloat { gutter - T.gutter + end + dockUnder }
    var shelfFade: CGFloat { T.shelfFade * dockShown }
    var shelfGap: CGFloat { T.shelfGap * dockShown }
}

extension EnvironmentValues {
    @Entry var pageContext = PageContext()
}

enum PagePadding {
    static func bottom(hasBar: Bool) -> CGFloat { T.px(48) + (hasBar ? T.barH + T.windowInset + T.px(24) : 0) }
    static var top: CGFloat { T.topbarH + T.contentTop }
}

/// A page's vertical scroll area. `overlay` lets the first section run under the top bar.
struct PageScroll<Content: View>: View {
    /// The artist photo's glow. It scrolls with the content and starts at the window's left edge.
    var glow: URL? = nil
    /// Nil for a page under the top bar. A value places the page below other content, clipped at its top.
    var top: CGFloat? = nil
    @ViewBuilder var content: Content
    @Environment(\.pageContext) private var ctx
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        // padding, not a content inset: NSScrollView keeps its inset for itself, so content under the top bar took no clicks.
        let inner = VStack(alignment: .leading, spacing: T.sectionGap) {
            Group(subviews: content) { sections in
                ForEach(Array(sections.enumerated()), id: \.element.id) { index, section in
                    section.modifier(LandSection(index: index, pageSince: ctx.pageSince))
                }
            }
        }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.top, top ?? T.topbarH + T.contentTop)
            .padding(.bottom, PagePadding.bottom(hasBar: ctx.hasBar))
            .padding(.leading, ctx.sidebarUnder)
            .padding(.trailing, ctx.dockUnder)
            .background(alignment: .top) { if let glow { PageGlow(url: glow, windowHeight: ctx.windowHeight) } }
        if isSnapshot {
            inner.frame(maxHeight: .infinity, alignment: .top).clipped()
        } else if top != nil {
            ScrollView(.vertical) { inner }
                .scrollIndicators(.never)
                .scrollEdgeEffectHidden(true, for: .top)
                .clipped()
        } else {
            ScrollView(.vertical) { inner }
                .scrollIndicators(.never)
                .scrollEdgeEffectHidden(true, for: .top)
                .scrolledTopFade()
        }
    }
}

/// The artwork tint behind a page's top. Fades to clear so it works over the translucent window.
struct PageTint: View {
    let hex: String
    @Environment(\.pageContext) private var ctx

    var body: some View {
        let base = RGBA(hex: hex) ?? T.bg
        LinearGradient(stops: [.init(color: base.opacity(0.56).color, location: 0),
                               .init(color: base.opacity(0.34).color, location: 0.62),
                               .init(color: .clear, location: 1)], startPoint: .top, endPoint: .bottom)
            .frame(height: T.px(460))
            .frame(maxWidth: .infinity)
            .mask(HStack(spacing: 0) {
                LinearGradient(colors: [.clear, .black], startPoint: .leading, endPoint: .trailing).frame(width: ctx.sidebarUnder)
                Color.black
            })
            .allowsHitTesting(false)
    }
}

/// The blur and fade under the top bar for content SwiftUI's scroll edge effect can't reach.
struct TopEdgeFade: View {
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        if !isSnapshot {
            EdgeBlurView()
                .frame(height: T.topbarH + T.px(20))
                .frame(maxWidth: .infinity)
                .allowsHitTesting(false)
        }
    }
}

/// The blur itself, faded out over its lower 45%. It drops the material's tint so flat pages stay flat.
/// SwiftUI's materials are window-server backdrops in a translucent window and can lag the content by a frame,
/// so this uses AppKit's effect with the backdrop kept in the app. `.active` keeps it the same when unfocused.
struct EdgeBlurView: NSViewRepresentable {
    func makeNSView(context: Context) -> EdgeBlurNSView { EdgeBlurNSView() }
    func updateNSView(_ nsView: EdgeBlurNSView, context: Context) {}
}

final class EdgeBlurNSView: NSView {
    private let effect = InAppEffectView()
    private let fade = CAGradientLayer()

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        effect.blendingMode = .withinWindow
        effect.material = .fullScreenUI
        effect.state = .active
        effect.blurOnly = true
        addSubview(effect)
        // Layer y runs up in this view, so the gradient starts at the top.
        fade.colors = [NSColor.black.cgColor, NSColor.black.cgColor, NSColor.clear.cgColor]
        fade.locations = [0, 0.55, 1]
        fade.startPoint = CGPoint(x: 0.5, y: 1)
        fade.endPoint = CGPoint(x: 0.5, y: 0)
        layer?.mask = fade
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func layout() {
        super.layout()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        effect.frame = bounds
        fade.frame = bounds
        CATransaction.commit()
    }
}

/// AppKit's within-window effect with its backdrop rendered in the app instead of by the window server.
/// `InAppBackdrops` sets this only where the layer has the key, so other macOS versions keep AppKit's behavior.
/// It's applied again after AppKit's own updates, which may rebuild the layers.
final class InAppEffectView: NSVisualEffectView {
    /// Keeps only the backdrop's blur, dropping its color filters and tint layers.
    var blurOnly = false { didSet { renderInApp() } }

    override func viewDidMoveToWindow() { super.viewDidMoveToWindow(); renderInApp() }
    override func layout() { super.layout(); renderInApp() }
    override func updateLayer() { super.updateLayer(); renderInApp() }
    override func viewDidChangeEffectiveAppearance() { super.viewDidChangeEffectiveAppearance(); renderInApp() }

    private func renderInApp() {
        guard let root = layer else { return }
        let blurOnly = blurOnly
        /// Whether this layer is or holds a backdrop. With `blurOnly`, every other layer is hidden.
        @discardableResult func walk(_ layer: CALayer) -> Bool {
            var holdsBackdrop = false
            if InAppBackdrops.isBackdrop(layer) {
                holdsBackdrop = true
                InAppBackdrops.keepInApp(layer)
                if blurOnly, let filters = layer.filters, filters.contains(where: { !Self.isBlur($0) }) {
                    layer.filters = filters.filter(Self.isBlur)
                }
            }
            for sub in layer.sublayers ?? [] where walk(sub) { holdsBackdrop = true }
            if blurOnly, !holdsBackdrop, layer !== root, !layer.isHidden { layer.isHidden = true }
            return holdsBackdrop
        }
        walk(root)
    }

    private static func isBlur(_ filter: Any) -> Bool {
        let object = filter as AnyObject
        let type = (object.responds(to: NSSelectorFromString("type")) ? object.value(forKey: "type") as? String : nil) ?? ""
        let name = (object.responds(to: NSSelectorFromString("name")) ? object.value(forKey: "name") as? String : nil) ?? ""
        return type.localizedCaseInsensitiveContains("blur") || name.localizedCaseInsensitiveContains("blur")
    }
}

/// Fades `TopEdgeFade` in across the window's top once a scroll view moves. The opacity follows the scroll
/// position over a short ramp, because an animated switch at a threshold can flicker back and forth.
private struct ScrolledTopFade: ViewModifier {
    @State private var opacity: Double = 0
    @Environment(\.pageContext) private var ctx

    /// Over how many points the fade comes in.
    private var ramp: CGFloat { T.px(28) }

    func body(content: Content) -> some View {
        content
            .onScrollGeometryChange(for: Double.self, of: { geo in
                let moved = geo.contentOffset.y + geo.contentInsets.top
                // Steps of 1/32 so scrolling well past the ramp doesn't update the state every frame.
                return (Double(max(0, min(1, moved / ramp))) * 32).rounded() / 32
            }) { _, value in
                opacity = value
                #if DEBUG
                EdgeFadeLog.record(value)
                #endif
            }
            .overlay(alignment: .top) {
                TopEdgeFade()
                    // Runs the full window width so no edge shows beside the sidebar or the dock.
                    // Never quite 0, or SwiftUI drops the material's backdrop layer and rebuilds it each time the page leaves the top.
                    .opacity(max(opacity, 0.001))
            }
    }
}

#if DEBUG
/// Every change of the scroll fade's opacity with its time, for CoreCheck's `edgeLog`.
@MainActor enum EdgeFadeLog {
    static var entries: [(CFTimeInterval, Double)] = []
    static func record(_ value: Double) {
        entries.append((CACurrentMediaTime(), value))
        if entries.count > 4000 { entries.removeFirst(1000) }
    }
}
#endif

extension View {
    func scrolledTopFade() -> some View { modifier(ScrolledTopFade()) }
}

/// A section heading that can open a page.
struct SectionTitle: View {
    let title: String
    var action: (() -> Void)? = nil

    var body: some View {
        if let action {
            Button(action: action) {
                HStack(spacing: T.px(6)) {
                    label
                    Image(systemName: "chevron.right").font(.system(size: T.px(17), weight: .bold)).foregroundStyle(T.t2.color)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
        } else {
            label
        }
    }

    private var label: some View {
        Text(title).font(.navia(T.textSection, .semibold)).tracking(-0.012 * T.textSection)
    }
}

/// A titled, horizontally scrolling row of cards that runs under the sidebar and fades out before the dock.
struct Shelf<Content: View>: View {
    let title: String
    var action: (() -> Void)? = nil
    var tiles = false
    /// Builds cards only as they come near the screen, for very long shelves.
    var lazy = false
    /// A quiet line under the title.
    var note: String? = nil
    /// A small glass button at the heading's right, such as "See Discography".
    var more: ShelfMore? = nil
    @ViewBuilder var content: Content
    @Environment(\.pageContext) private var ctx

    var body: some View {
        VStack(alignment: .leading, spacing: T.shelfHeadGap) {
            HStack(alignment: .center, spacing: T.shelfHeadGap) {
                SectionHeading(title: title, note: note, action: action)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if let more { ActionButton(title: more.title, small: true, action: more.action) }
            }
            .padding(.leading, ctx.gutter)
            .padding(.trailing, more == nil ? 0 : ctx.gutter)
            HScrollRow(leading: ctx.gutter + ctx.shelfUnder, trailing: ctx.gutter + ctx.shelfFade, spacing: tiles ? T.tileGap : T.cardGap,
                       arrowLeading: ctx.shelfUnder + T.px(10), trailingFade: ctx.shelfFade, trailingGap: ctx.shelfGap, lazy: lazy) { content }
                .padding(.leading, -ctx.shelfUnder)
        }
    }
}

struct SectionHeading: View {
    let title: String
    var note: String? = nil
    var action: (() -> Void)? = nil

    var body: some View {
        VStack(alignment: .leading, spacing: T.px(2)) {
            SectionTitle(title: title, action: action)
            if let note, !note.isEmpty { Text(note).font(.navia(T.textSM)).foregroundStyle(T.t2.color) }
        }
    }
}

/// [`Shelf`]'s heading button.
struct ShelfMore {
    let title: String
    let action: () -> Void
}

/// a mask, opaque throughout at 0. it stays in place at 0, since adding or dropping a mask rebuilds the whole row.
private struct TrailingFade: View {
    let fade: CGFloat
    let gap: CGFloat

    var body: some View {
        GeometryReader { geo in
            let w = max(1, geo.size.width)
            LinearGradient(stops: [.init(color: .black, location: max(0, 1 - (fade + gap) / w)),
                                   .init(color: .black.opacity(0.62), location: max(0, 1 - (fade * 0.62 + gap) / w)),
                                   .init(color: .black.opacity(0.22), location: max(0, 1 - (fade * 0.28 + gap) / w)),
                                   .init(color: .clear, location: max(0, 1 - gap / w))],
                           startPoint: .leading, endPoint: .trailing)
        }
    }
}

struct HScrollRow<Content: View>: View {
    var leading: CGFloat
    var trailing: CGFloat
    var spacing: CGFloat
    var arrowTop: CGFloat? = nil
    /// The left arrow's offset from the row's left edge. Shelves pass under the sidebar, so their arrow sits just
    /// past the sidebar's edge.
    var arrowLeading: CGFloat? = nil
    /// A shelf's fade into the dock. It fades the cards only, and the right arrow stays at full strength.
    var trailingFade: CGFloat = 0
    var trailingGap: CGFloat = 0
    /// A soft edge this wide on whichever side has more to scroll. Zero for none.
    var edgeFade: CGFloat = 0
    /// A `LazyHStack`, so only the cards near the screen are built.
    var lazy = false
    @ViewBuilder var content: Content
    @State private var position = ScrollPosition(idType: Int.self)
    @State private var geo = RowGeometry()
    @State private var hovering = false
    @Environment(\.pageContext) private var ctx
    @Environment(\.isSnapshot) private var isSnapshot

    struct RowGeometry: Equatable {
        var offset: CGFloat = 0
        var content: CGFloat = 0
        var container: CGFloat = 0
        var canLeft: Bool { offset > 1 }
        var canRight: Bool { offset < content - container - 1 }
    }

    var body: some View {
        let row = stack
            .padding(.leading, leading)
            .padding(.trailing, trailing)
        if isSnapshot {
            // A fixed width: unconstrained, the row's natural width would widen the whole page in the offscreen render.
            row.frame(width: max(1, ctx.pageWidth + ctx.shelfUnder), alignment: .leading).clipped().mask(TrailingFade(fade: trailingFade, gap: trailingGap))
        } else {
            ScrollView(.horizontal) { row }
                .scrollIndicators(.never)
                .scrollPosition($position)
                .onScrollGeometryChange(for: RowGeometry.self) { g in
                    RowGeometry(offset: g.contentOffset.x, content: g.contentSize.width, container: g.containerSize.width)
                } action: { _, new in geo = new }
                .mask {
                    Group { if edgeFade > 0 { edgeMask } else { Rectangle() } }
                        .mask(TrailingFade(fade: trailingFade, gap: trailingGap))
                }
                .overlay(alignment: .topLeading) { if geo.canLeft && (hovering || HoverDebug.forced("ShelfArrows")) { arrow("chevron.left", -1).padding(.leading, arrowLeading ?? leading - T.controlH / 2 - T.px(9)).padding(.top, arrowInset) } }
                .overlay(alignment: .topTrailing) { if geo.canRight && (hovering || HoverDebug.forced("ShelfArrows")) { arrow("chevron.right", 1).padding(.trailing, T.px(10) + trailingGap).padding(.top, arrowInset) } }
                .onHover { hovering = $0 }
                #if DEBUG
                // With `-naviaForceHover ShelfArrows` rows start scrolled, so the left arrow shows.
                .task { if HoverDebug.forced("ShelfArrows") { try? await Task.sleep(for: .milliseconds(600)); position.scrollTo(x: T.px(400)) } }
                #endif
        }
    }

    @ViewBuilder private var stack: some View {
        if lazy { LazyHStack(alignment: .top, spacing: spacing) { content } } else { HStack(alignment: .top, spacing: spacing) { content } }
    }

    private var arrowInset: CGFloat { arrowTop ?? T.px(84) }

    /// Clear for the outer quarter of the fade, then up to opaque.
    private var edgeMask: some View {
        HStack(spacing: 0) {
            if geo.canLeft { fade(.leading) }
            Rectangle()
            if geo.canRight { fade(.trailing) }
        }
    }

    private func fade(_ edge: HorizontalEdge) -> some View {
        let stops: [Gradient.Stop] = [.init(color: .clear, location: 0), .init(color: .clear, location: 0.25), .init(color: .black, location: 1)]
        return LinearGradient(stops: stops, startPoint: edge == .leading ? .leading : .trailing, endPoint: edge == .leading ? .trailing : .leading)
            .frame(width: edgeFade)
    }

    private func arrow(_ symbol: String, _ direction: CGFloat) -> some View {
        RoundArrowButton(symbol: symbol, label: direction < 0 ? L("Scroll left") : L("Scroll right"), key: "ShelfArrow") {
            let target = min(max(0, geo.offset + direction * max(200, geo.container * 0.85)), max(0, geo.content - geo.container))
            withAnimation(T.easeOut(T.shelfScroll)) { position.scrollTo(x: target) }
        }
    }
}

/// the round glass arrow at a scrolling row's edge: a shelf's, the cover row's.
struct RoundArrowButton: View {
    let symbol: String
    let label: String
    let key: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol).font(.system(size: T.px(15), weight: .bold))
                .frame(width: T.controlH, height: T.controlH).contentShape(Circle())
        }
        .buttonStyle(HoverFillStyle(key: key))
        .glassPill()
        .help(label)
        .accessibilityLabel(label)
    }
}

/// columns of at least `cell` and the width each stretches to (GPUI `grid_fit`).
func gridFit(width: CGFloat, cell: CGFloat, gap: CGFloat) -> (columns: Int, width: CGFloat) {
    let columns = max(1, Int((width + gap) / (cell + gap)))
    return (columns, (width - gap * CGFloat(columns - 1)) / CGFloat(columns))
}

/// lazily built rows, so a library of any size only creates what's on screen; `cell` gets the index and card width.
struct LazyCardGrid<Cell: View>: View {
    let count: Int
    var cardWidth: CGFloat = T.gridCard
    var rowSpacing: CGFloat = T.gridRowGap
    var gutter: CGFloat = T.gutter
    @ViewBuilder let cell: (Int, CGFloat) -> Cell
    @Environment(\.pageContext) private var ctx
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        let usable = max(cardWidth, (ctx.layoutWidth > 0 ? ctx.layoutWidth : ctx.pageWidth) - 2 * gutter)
        let fit = gridFit(width: usable, cell: cardWidth, gap: T.cardGap)
        let rows = (count + fit.columns - 1) / fit.columns
        Group {
            if isSnapshot {
                VStack(alignment: .leading, spacing: rowSpacing) { rowsView(0..<min(rows, 12), columns: fit.columns, width: fit.width) }
            } else {
                LazyVStack(alignment: .leading, spacing: rowSpacing) { rowsView(0..<rows, columns: fit.columns, width: fit.width) }
            }
        }
        .padding(.leading, gutter)
        .padding(.trailing, gutter)
    }

    private func rowsView(_ range: Range<Int>, columns: Int, width: CGFloat) -> some View {
        ForEach(range, id: \.self) { row in
            HStack(alignment: .top, spacing: T.cardGap) {
                ForEach(row * columns..<min(row * columns + columns, count), id: \.self) { index in
                    cell(index, width).frame(width: width, alignment: .topLeading)
                }
            }
        }
    }
}

/// The message for a page or list with nothing to show.
struct EmptyStateView: View {
    var symbol: String? = nil
    let title: String
    var hint: String? = nil
    var compact = false
    var actionTitle: String? = nil
    var action: (() -> Void)? = nil
    /// Shown under the hint, such as a sync's progress.
    var accessory: AnyView? = nil

    var body: some View {
        VStack(spacing: T.px(8)) {
            if let symbol {
                Image(systemName: symbol).font(.system(size: T.px(compact ? 26 : 34))).foregroundStyle(T.t3.color).padding(.bottom, T.px(4))
            }
            Text(title).font(.navia(T.textLG, .semibold)).foregroundStyle(T.t1.color).multilineTextAlignment(.center)
            if let hint {
                Text(hint).font(.navia(T.textMD)).foregroundStyle(T.t2.color).multilineTextAlignment(.center).lineSpacing(T.px(2)).frame(maxWidth: T.px(380))
            }
            if let actionTitle, let action {
                ActionButton(title: actionTitle, action: action)
                    .padding(.top, T.px(8))
            }
            if let accessory { accessory.padding(.top, T.px(8)) }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, compact ? T.px(36) : T.px(80))
        .padding(.horizontal, T.gutter)
    }
}

/// What an empty page says. While the library isn't ready it says why, otherwise the page's own wording.
struct LibraryEmptyState: View {
    let library: any LibraryViewModel
    var symbol: String? = nil
    let title: String
    var hint: String? = nil

    var body: some View {
        switch library.state {
        case .ready: EmptyStateView(symbol: symbol, title: title, hint: hint)
        case .opening: Color.clear.frame(height: T.px(240))
        case .noServer: EmptyStateView(symbol: "music.note.list", title: L("No server connected"),
                                       hint: L("Navia plays music from a Navidrome server. Sign in from Settings."))
        case .syncing: EmptyStateView(symbol: "music.note.list", title: L("Syncing library"), hint: L("Music shows up here as soon as it arrives."),
                                      accessory: AnyView(SyncPageMeter(library: library)))
        case .unreachable: EmptyStateView(symbol: "exclamationmark.triangle.fill", title: L("Can't reach the server"), hint: L("Check the address and your connection. The music shows up once Navia can connect."))
        case .syncFailed: EmptyStateView(symbol: "exclamationmark.triangle.fill", title: L("Couldn't finish syncing"), hint: L("Part of your library may be missing. Navia will try again."))
        }
    }
}

/// Creates a page's list model once for the life of the page.
@MainActor final class Once<Value> {
    private var value: Value?
    func get(_ make: () -> Value) -> Value {
        if let value { return value }
        let made = make()
        value = made
        return made
    }
}

/// A page header's action row. Trailing controls sit at the right end, or wrap to their own line when there's no
/// room. `pushTrailing: false` keeps them right after the leading ones.
struct ActionRow<Leading: View, Trailing: View>: View {
    var spacing: CGFloat
    var pushTrailing = true
    @ViewBuilder let leading: Leading
    @ViewBuilder let trailing: Trailing

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: spacing) {
                leading
                if pushTrailing { Spacer(minLength: 0) }
                trailing
            }
            VStack(alignment: .leading, spacing: spacing) {
                HStack(spacing: spacing) { leading }
                HStack(spacing: spacing) { trailing }
            }
        }
    }
}
