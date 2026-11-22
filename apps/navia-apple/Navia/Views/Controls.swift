import AppKit
import SwiftUI

extension View {
    /// the system's own glass with no Navia tint or edge, so the system's look and transparency settings apply.
    func glassPill(interactive: Bool = false) -> some View {
        modifier(NativeGlass(shape: Capsule(), interactive: interactive))
    }

    /// the system's own glass in `shape`, as `glassPill`; `flat` is what offscreen check images draw instead.
    func plainGlass(_ shape: some Shape, flat: RGBA) -> some View {
        modifier(NativeGlass(shape: shape, flat: flat))
    }

    /// the system's prominent glass button: the glass tinted with `tint` (the accent, or `--danger`).
    func prominentGlassPill(_ tint: RGBA) -> some View {
        modifier(NativeGlass(shape: Capsule(), tint: tint, interactive: true))
    }

    /// `veil` sits the glass closer to the window's color; `inactiveVeil` replaces it while the platform glass lifts itself.
    func glassPanel(radius: CGFloat, tint: RGBA, veil: RGBA? = nil, inactiveVeil: RGBA? = nil) -> some View {
        modifier(GlassFill(shape: AnyShape(RoundedRectangle(cornerRadius: radius, style: .continuous)), tint: tint, veil: veil, inactiveVeil: inactiveVeil))
    }

    /// The panel glass of the sidebar and Settings' cards. Its edge is the platform glass's own.
    func panelGlass(radius: CGFloat) -> some View {
        glassPanel(radius: radius, tint: T.panelGlass, veil: T.panelGlassVeil, inactiveVeil: T.panelGlassVeilInactive)
    }

    /// `--panel` as a plain fill: grouped rows such as Settings, which have no glass rim.
    func panelPlain(radius: CGFloat) -> some View {
        background(T.panel.color, in: RoundedRectangle(cornerRadius: radius, style: .continuous))
    }

    /// A floating panel on the system's glass with `--shadow-float`; `clip` keeps scrolling content inside. `tinted` is
    /// Navia's `--float-glass` with `--glass-edge`, for a card standing on its own (the sign-in form, the queue's prompt card).
    func floatPanel(clip: Bool = false, radius: CGFloat = T.rFloat, tinted: Bool = false) -> some View {
        modifier(FloatPanel(clip: clip, radius: radius, tinted: tinted))
    }

    func shadow(_ token: T.Shadow) -> some View {
        shadow(color: token.color.color, radius: T.px(token.blur / 2), x: T.px(token.x), y: T.px(token.y))
    }

    /// The accent ring just outside a focused field or an open select. An inset border would shift the contents.
    func focusRing(_ on: Bool, _ color: RGBA, width: CGFloat = T.focusRingWidth) -> some View {
        overlay(Capsule().strokeBorder(color.color, lineWidth: width).padding(-width).opacity(on ? 1 : 0).allowsHitTesting(false))
    }

    /// `--invalid-outline`: the thin `--invalid-ring` round a field or value with a problem.
    func invalidRing(_ on: Bool) -> some View { focusRing(on, T.invalidRing, width: T.invalidRingW) }

    func artShadow() -> some View { shadow(T.shadowArt) }
}

/// A one-time question inside a panel, such as the queue's Deezer card. A title, one fact beside its glyph, then
/// the answers side by side.
struct PromptCard<Answers: View>: View {
    let title: String
    let symbol: String
    let fact: String
    @ViewBuilder let answers: Answers

    var body: some View {
        VStack(alignment: .leading, spacing: T.promptCardGap) {
            Text(title).font(.navia(T.textMD, .semibold)).foregroundStyle(T.tEm.color).fixedSize(horizontal: false, vertical: true)
            HStack(alignment: .firstTextBaseline, spacing: T.promptCardGap) {
                Image(systemName: symbol).font(.system(size: T.textSM)).foregroundStyle(T.t2.color)
                Text(fact).font(.navia(T.textSM)).foregroundStyle(T.t2.color).fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: T.dialogActionsGap) { answers }
        }
        .padding(T.promptCardPad)
        .frame(maxWidth: .infinity, alignment: .leading)
        .floatPanel(radius: T.rLG, tinted: true)
    }
}

private struct FloatPanel: ViewModifier {
    let clip: Bool
    /// `T.rFloat`, or `T.rLg` for a card standing on its own.
    let radius: CGFloat
    let tinted: Bool

    func body(content: Content) -> some View {
        let shape = RoundedRectangle(cornerRadius: radius, style: .continuous)
        Group {
            if clip { glass(content, shape).clipShape(shape) } else { glass(content, shape) }
        }
        .overlay { if tinted { shape.strokeBorder(T.glassEdge.color, lineWidth: 1).allowsHitTesting(false) } }
        .shadow(T.shadowFloat)
    }

    @ViewBuilder private func glass(_ content: Content, _ shape: RoundedRectangle) -> some View {
        if tinted { content.glassPanel(radius: radius, tint: T.floatGlass) } else { content.plainGlass(shape, flat: T.menuGlass) }
    }
}

/// offscreen check images can't draw glass, so they get its flat tint instead.
private struct GlassFill: ViewModifier {
    let shape: AnyShape
    let tint: RGBA
    var veil: RGBA? = nil
    var inactiveVeil: RGBA? = nil
    @Environment(\.isSnapshot) private var isSnapshot
    @Environment(\.appearsActive) private var active

    func body(content: Content) -> some View {
        let shown = active ? veil : (inactiveVeil ?? veil)
        let veiled = content.background { if let shown { shape.fill(shown.color) } }
        if isSnapshot {
            veiled.background(shape.fill(tint.color))
        } else {
            veiled.glassEffect(.regular.tint(tint.color), in: shape)
        }
    }
}

/// offscreen check images can't draw glass, so they get a flat fill (`--glass` unless `flat` says otherwise).
private struct NativeGlass<S: Shape>: ViewModifier {
    let shape: S
    var tint: RGBA? = nil
    var interactive = false
    var flat: RGBA? = nil
    @Environment(\.isSnapshot) private var isSnapshot

    func body(content: Content) -> some View {
        if isSnapshot {
            content.background(shape.fill((flat ?? tint ?? T.glass).color))
        } else {
            content.glassEffect(.regular.tint(tint?.color).interactive(interactive), in: shape)
        }
    }
}

struct GlassGroup<Content: View>: View {
    @ViewBuilder var content: Content
    var body: some View {
        HStack(spacing: 0) { content }
            .padding(.horizontal, T.px(3))
            .frame(height: T.controlH)
            .glassPill()
    }
}

/// what a page's refresh does: `library` runs the library sync now and turns with any sync; `rows` reloads the rows of
/// Home, New, Charts and Radio.
enum PageRefresh { case library, rows }

/// a page's refresh, the last of its controls, in its own pill: `arrow.clockwise` turning once per `T.spinPeriod` while
/// a refresh runs, ending on a whole turn; still while motion is reduced. a click while it turns does nothing.
struct RefreshButton: View {
    let library: any LibraryViewModel
    let kind: PageRefresh
    @State private var turnStart: Date?
    @State private var turnEnd: Date?

    var body: some View {
        let busy = library.isRefreshing(library: kind == .library)
        GlassGroup {
            IconButton(symbol: "arrow.clockwise", label: L("Refresh"), glyph: AnyView(glyph)) { if !busy { library.refreshNow() } }
        }
        .onChange(of: busy, initial: true) { _, on in turn(on) }
    }

    private var glyph: some View {
        TimelineView(.animation(paused: Motion.shared.reduced || turnStart == nil)) { context in
            Image(systemName: "arrow.clockwise")
                .font(.system(size: T.px(17)))
                .rotationEffect(.degrees(angle(at: context.date)))
        }
    }

    private func angle(at now: Date) -> Double {
        guard let turnStart, !Motion.shared.reduced, turnEnd.map({ now < $0 }) ?? true else { return 0 }
        return now.timeIntervalSince(turnStart).truncatingRemainder(dividingBy: T.spinPeriod) / T.spinPeriod * 360
    }

    private func turn(_ on: Bool) {
        let now = Date()
        if on {
            if turnStart == nil { turnStart = now }
            turnEnd = nil
        } else if let start = turnStart {
            let end = start.addingTimeInterval(max(1, ceil(now.timeIntervalSince(start) / T.spinPeriod)) * T.spinPeriod)
            turnEnd = end
            Task { @MainActor in
                try? await Task.sleep(for: .seconds(end.timeIntervalSince(now)))
                if turnEnd == end { turnStart = nil; turnEnd = nil }
            }
        }
    }
}

struct PlayActions<Extra: View>: View {
    let songs: [SongRow]
    let shell: ShellModel
    @ViewBuilder var extra: Extra

    var body: some View {
        let none = songs.isEmpty
        GlassGroup {
            TextPillButton(symbol: "play.fill", title: L("Play"), disabled: none, key: "Play") { shell.playAll(songs) }
                .help(none ? L("No songs to play") : L("Play"))
            GlassDivider()
            IconButton(symbol: "shuffle", label: none ? L("No songs to play") : L("Shuffle"), disabled: none) { shell.playAll(songs, shuffle: true) }
        }
        if Extra.self != EmptyView.self { GlassGroup { extra } }
    }
}

/// A text button inside a glass group, with a filled symbol and a label. Disabled ones are dimmed.
struct TextPillButton: View {
    /// Nil for a word alone.
    let symbol: String?
    let title: String
    var disabled = false
    /// The name `-naviaForceHover` knows it by.
    var key: String? = nil
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: T.px(9)) {
                if let symbol { Image(systemName: symbol).font(.system(size: T.px(16))) }
                Text(title).font(.navia(T.textMD, .medium))
            }
            .padding(.leading, T.px(13))
            .padding(.trailing, T.px(12))
            .frame(height: T.iconButtonH)
            .contentShape(Capsule())
        }
        .buttonStyle(HoverFillStyle(key: key ?? title))
        .disabled(disabled)
        .opacity(disabled ? T.disabledOpacity : 1)
    }
}

/// An action button in a dialog, form or page, using the system's glass buttons. `.prominent` is tinted glass and
/// `.secondary` is plain glass, with system hover, pressed and disabled looks. `compact` is the smaller size.
/// `small` is drawn by Navia, since the system's glass buttons have no size that matches it.
struct ActionButton: View {
    /// `.primary` is a solid `fill` with `ink` text. Navia draws it when `small`, and uses prominent glass otherwise.
    enum Kind { case secondary, prominent(tint: RGBA), primary(fill: RGBA, ink: RGBA) }
    let title: String
    var symbol: String? = nil
    var kind: Kind = .secondary
    var compact = false
    var small = false
    var enabled = true
    /// Shares the row's width with its neighbors.
    var fill = false
    var busy = false
    let action: () -> Void

    var body: some View {
        if small {
            Button(action: action) {
                HStack(spacing: T.buttonGap) {
                    if let symbol { Image(systemName: symbol).font(.system(size: T.chipGlyph)) }
                    Text(title).lineLimit(1)
                }
                    .font(.navia(T.textXS, isPrimary ? .semibold : .medium))
                    .fixedSize()
                    .padding(.horizontal, T.buttonSmallPad)
                    .frame(maxWidth: fill ? .infinity : nil)
                    .frame(height: T.buttonSmallH)
                    .modifier(SmallButtonFill(kind: kind, enabled: enabled))
                    .contentShape(Capsule())
            }
            .buttonStyle(HoverFillStyle(key: title, shrinks: isPrimary))
            .disabled(!enabled)
            // A disabled primary keeps a faint fill and a legible label instead of fading whole.
            .opacity(enabled || isPrimary ? 1 : T.disabledOpacity)
        } else {
            system
        }
    }

    private var isPrimary: Bool { if case .primary = kind { return true }; return false }

    @ViewBuilder private var system: some View {
        let button = Button(action: action) {
            HStack(spacing: T.buttonGap) {
                if busy { Spinner(color: T.tEm).frame(width: T.px(16), height: T.px(16)) }
                if let symbol { Image(systemName: symbol) }
                Text(title).lineLimit(1)
            }
            .font(.navia(compact ? T.textSM : T.textMD, .medium))
            .fixedSize()
            .frame(maxWidth: fill ? .infinity : nil)
        }
        .controlSize(compact ? .large : .extraLarge)
        .disabled(!enabled)
        switch kind {
        case .secondary: button.buttonStyle(.glass)
        case .prominent(let tint), .primary(let tint, _): button.buttonStyle(.glassProminent).tint(tint.color)
        }
    }
}

/// A small button's fill. A disabled primary keeps `disabledOpacity` of its fill with the `t2` label.
private struct SmallButtonFill: ViewModifier {
    let kind: ActionButton.Kind
    let enabled: Bool
    func body(content: Content) -> some View {
        switch kind {
        case .secondary: content.glassPill(interactive: true)
        case .prominent(let tint): content.prominentGlassPill(tint)
        case .primary(let fill, let ink):
            content
                .foregroundStyle((enabled ? ink : T.t2).color)
                .background(Capsule().fill(fill.opacity(enabled ? 1 : T.disabledOpacity).color))
                .overlay(Capsule().strokeBorder(T.glassEdge.color, lineWidth: 1).allowsHitTesting(false))
        }
    }
}

struct GlassDivider: View {
    var body: some View {
        Rectangle().fill(T.glassDivider.color).frame(width: 1, height: T.px(20)).padding(.horizontal, T.px(3))
    }
}

struct IconButton: View {
    let symbol: String
    let label: String
    var size: CGFloat = T.px(17)
    var weight: Font.Weight = .regular
    var active = false
    var inGlass = false
    var width: CGFloat = T.iconW
    var height: CGFloat = T.iconButtonH
    var disabled = false
    /// The tint for shuffle and repeat when on.
    var activeTint: Color? = nil
    /// A drawn glyph in place of the symbol, such as the Download switch's progress ring.
    var glyph: AnyView? = nil
    var help: String? = nil
    /// Window controls over a cover, on a dark fill that darkens on hover.
    var onArt = false
    /// the glyph pops as the button turns on, as a heart does when liking.
    var pop = false
    /// 0 to 1 for a symbol drawn by level (the volume's waves); a change of symbol morphs into the next one.
    var level: Double? = nil
    var action: () -> Void = {}
    @State private var hovering = false
    @State private var pops = 0
    @Environment(\.springPress) private var spring

    var body: some View {
        Button {
            if pop && !active && !Motion.isReduced { pops += 1 }
            action()
        } label: {
            Group {
                if let glyph {
                    glyph
                } else {
                    Image(systemName: symbol, variableValue: level)
                        .font(.system(size: size, weight: weight))
                        // draw mode grows each wave with the level instead of switching it on whole.
                        .symbolVariableValueMode(.draw)
                        .contentTransition(.symbolEffect(.automatic))
                        .animation(level == nil ? nil : T.toggle, value: symbol)
                        .animation(level == nil ? nil : T.toggle, value: level)
                        .symbolVariant(active && weight != .semibold && weight != .bold ? .fill : .none)
                        .foregroundStyle(active && activeTint != nil ? AnyShapeStyle(activeTint!) : AnyShapeStyle(.foreground))
                        .modifier(LikePopEffect(trigger: pops))
                }
            }
                .frame(width: width, height: height)
                .background(Capsule().fill(active && inGlass ? T.glassOn.color : .clear))
                .background(Capsule().fill(onArt ? (hovering ? T.windowButtonOnArtHover : T.windowButtonOnArt).color : .clear))
                .animation(T.toggle, value: active)
                .contentShape(Capsule())
        }
        .buttonStyle(HoverBrightenStyle(selected: active && inGlass, key: label, glyph: size))
        .opacity(disabled ? 0.3 : (active || hovering || spring || HoverDebug.forced(label) ? 1 : T.iconRest))
        .disabled(disabled)
        .onHover { hovering = $0 }
        .help(help ?? label)
        .accessibilityLabel(label)
    }

}

/// Hands wheel events over a hovered slider to that slider. It uses a local monitor because SwiftUI has no wheel
/// event on the Mac and an AppKit overlay would take the slider's drags.
@MainActor final class WheelRouter {
    static let shared = WheelRouter()
    private var handler: ((Int) -> Void)?
    private var monitor: Any?
    private var pixels: CGFloat = 0

    func enter(_ handler: @escaping (Int) -> Void) {
        self.handler = handler
        pixels = 0
        guard monitor == nil else { return }
        monitor = NSEvent.addLocalMonitorForEvents(matching: .scrollWheel) { [weak self] event in
            guard let self, let handler = self.handler else { return event }
            for step in self.steps(event) { handler(step) }
            return nil
        }
    }

    func leave() { handler = nil; pixels = 0 }

    /// One step per wheel notch. Trackpad moves add up first, and momentum after the fingers lift is ignored.
    /// Scrolling up is louder.
    private func steps(_ event: NSEvent) -> [Int] {
        guard event.momentumPhase.isEmpty, event.scrollingDeltaY != 0 else { return [] }
        guard event.hasPreciseScrollingDeltas else { return [event.scrollingDeltaY > 0 ? 1 : -1] }
        pixels += event.scrollingDeltaY
        let threshold: CGFloat = 16
        guard abs(pixels) >= threshold else { return [] }
        let count = Int(abs(pixels) / threshold)
        pixels = pixels.truncatingRemainder(dividingBy: threshold)
        return Array(repeating: event.scrollingDeltaY > 0 ? 1 : -1, count: count)
    }
}

/// A thin track for seeking and volume. The thumb fades in on hover and stays while dragging.
struct TrackSlider: View {
    let value: Double
    let total: Double
    var rest: RGBA = T.barTrack
    /// The mini player's hairline track, with a smaller thumb.
    var thin = false
    /// the bar's line: `barScrubRest` thick, `barScrubHover` while hovered or dragged.
    var grows = false
    /// set by a host that decides when the line counts as hovered (the bar, after its delay); nil uses the pointer.
    var raised: Bool? = nil
    /// text in a float chip over the playhead while hovered, kept `T.scrubChipEdge` from the ends.
    var chip: String? = nil
    /// A wheel step while hovered, 1 up and -1 down. Nil leaves the wheel to whatever is behind the slider.
    var onWheel: ((Int) -> Void)? = nil
    let onChange: (Double) -> Void
    @State private var hovering = false
    @State private var dragging = false
    @State private var chipHeight: CGFloat = 0

    var body: some View {
        GeometryReader { geo in
            let fraction = total > 0 ? min(1, max(0, value / total)) : 0
            let shown = (raised ?? hovering) || dragging
            let restLine = thin ? T.scrubThin : grows ? T.barScrubRest : T.scrubLine
            let line = shown ? (thin ? T.scrubThinHover : grows ? T.barScrubHover : T.scrubLineHover) : restLine
            ZStack(alignment: .leading) {
                Capsule().fill(rest.color).frame(height: line)
                Capsule().fill(.foreground).frame(width: geo.size.width * fraction, height: line)
            }
            .animation(T.hover(shown), value: line)
            .frame(maxHeight: .infinity)
            .overlay(alignment: .topLeading) {
                if let chip {
                    let x = min(max(geo.size.width * fraction, T.scrubChipEdge), max(T.scrubChipEdge, geo.size.width - T.scrubChipEdge))
                    Text(chip)
                        .font(.navia(T.textXS).monospacedDigit())
                        .foregroundStyle(T.t1.color)
                        .lineLimit(1)
                        .padding(.horizontal, T.px(8))
                        .padding(.vertical, T.px(3))
                        .glassPill()
                        .fixedSize()
                        .alignmentGuide(.leading) { $0[HorizontalAlignment.center] - x }
                        // Sits 2 px above the bar. `.alignmentGuide(.top)` is ignored here, so the measured height places it.
                        .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { chipHeight = $0 }
                        .offset(y: -(chipHeight + T.px(2)))
                        .opacity(hovering || HoverDebug.forced("ScrubChip") ? 1 : 0)
                        .animation(T.hover(hovering), value: hovering)
                        .allowsHitTesting(false)
                }
            }
            .contentShape(Rectangle())
            .gesture(DragGesture(minimumDistance: 0).onChanged { drag in
                dragging = true
                onChange(min(1, max(0, drag.location.x / max(1, geo.size.width))) * total)
            }.onEnded { _ in dragging = false })
        }
        .frame(height: T.scrubSlot)
        .onHover { over in
            hovering = over
            guard let onWheel else { return }
            if over { WheelRouter.shared.enter(onWheel) } else { WheelRouter.shared.leave() }
        }
        .onDisappear { if hovering, onWheel != nil { WheelRouter.shared.leave() } }
    }
}

/// The system's switch, tinted with the accent. `label` is for VoiceOver.
struct ToggleSwitch: View {
    @Binding var isOn: Bool
    let accent: RGBA
    let label: String

    var body: some View {
        Toggle(label, isOn: $isOn)
            .toggleStyle(.switch)
            .labelsHidden()
            .tint(accent.color)
    }
}


/// The accent count pill on a drag card or a lifted row.
struct CountBadge: View {
    let text: String
    let accent: RGBA
    let onAccent: RGBA
    var body: some View {
        Text(text)
            .font(.navia(T.px(12), .semibold))
            .foregroundStyle(onAccent.color)
            .padding(.horizontal, T.px(8))
            .frame(height: T.px(20))
            .background(Capsule().fill(accent.color))
            .shadow(T.shadowBadge)
            .fixedSize()
    }
}

/// Lays items out left to right, wrapping when the width runs out, each line centered vertically. An item wider
/// than the line gets the line's width, so long text wraps.
struct FlowChips: Layout {
    let spacing: CGFloat
    var lineSpacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        arrange(subviews, width: proposal.width ?? .infinity).size
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let layout = arrange(subviews, width: bounds.width)
        for (index, frame) in layout.frames.enumerated() {
            subviews[index].place(at: CGPoint(x: bounds.minX + frame.minX, y: bounds.minY + frame.minY), proposal: ProposedViewSize(frame.size))
        }
    }

    private func arrange(_ subviews: Subviews, width: CGFloat) -> (frames: [CGRect], size: CGSize) {
        var lines: [[Int]] = [[]]
        var sizes: [CGSize] = []
        var x: CGFloat = 0
        for (index, sub) in subviews.enumerated() {
            var size = sub.sizeThatFits(.unspecified)
            if size.width > width { size = sub.sizeThatFits(ProposedViewSize(width: width, height: nil)) }
            if x > 0 && x + size.width > width { lines.append([]); x = 0 }
            lines[lines.count - 1].append(index)
            sizes.append(size)
            x += size.width + spacing
        }
        var frames = [CGRect](repeating: .zero, count: sizes.count)
        var y: CGFloat = 0, right: CGFloat = 0
        for (n, line) in lines.enumerated() where !line.isEmpty {
            let height = line.map { sizes[$0].height }.max() ?? 0
            var lx: CGFloat = 0
            for index in line {
                frames[index] = CGRect(x: lx, y: y + (height - sizes[index].height) / 2, width: sizes[index].width, height: sizes[index].height)
                lx += sizes[index].width + spacing
            }
            right = max(right, lx - spacing)
            y += height + (n < lines.count - 1 ? lineSpacing : 0)
        }
        return (frames, CGSize(width: right, height: y))
    }
}

/// Decides which chips of a one-line row show. The line never wraps or overflows. Kept chips show first, then the
/// others in order until one doesn't fit, and the rest fold into More. The end group (`endW`) sits at the right
/// end, and moves into More (`endInMore`) when the kept chips leave it no room. `pin` always shows, and `rest`
/// keeps More visible even when every chip fits.
enum ChipFold {
    static func fold(widths: [CGFloat], keep: [Bool], pin: Int = -1, moreW: CGFloat, endW: CGFloat, gap: CGFloat, room: CGFloat, rest: Bool) -> (shown: [Bool], more: Bool, endInMore: Bool) {
        let endRoom = endW > 0 ? gap + endW : 0
        let all = widths.reduce(0) { $0 + $1 + gap } - (widths.isEmpty ? 0 : gap)
        if !rest && all + endRoom <= room { return (widths.map { _ in true }, false, false) }
        let must = widths.indices.map { keep[$0] || $0 == pin }
        let kept = zip(widths, must).reduce(CGFloat(0)) { $0 + ($1.1 ? $1.0 + gap : 0) }
        let budget = room - endRoom - moreW
        if kept <= budget {
            var used = kept, folding = false
            let shown = zip(widths, must).map { w, m -> Bool in
                if m { return true }
                if !folding && used + w + gap <= budget { used += w + gap; return true }
                folding = true
                return false
            }
            return (shown, true, false)
        }
        var used = pin >= 0 && pin < widths.count ? widths[pin] + gap : 0, folding = false
        let shown = widths.indices.map { i -> Bool in
            if i == pin { return true }
            if keep[i] && !folding && used + widths[i] + gap <= room - moreW { used += widths[i] + gap; return true }
            if keep[i] { folding = true }
            return false
        }
        return (shown, true, endW > 0)
    }
}

struct ChipValue: View {
    let text: String
    var body: some View {
        Text(text).lineLimit(1).truncationMode(.tail).frame(maxWidth: T.chipValueMaxW, alignment: .leading).fixedSize(horizontal: false, vertical: true)
    }
}

/// A filter chip, `T.chipH` tall, glass at rest and `T.chipOn` when on. `menu` adds a caret for a popover, and
/// `remove` adds an X button that shares the chip's shape.
struct FilterChip<Label: View>: View {
    var on = false
    var menu = false
    var remove: (() -> Void)? = nil
    var removeLabel = ""
    let action: () -> Void
    @ViewBuilder var label: Label
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 0) {
            Button(action: action) {
                HStack(spacing: T.chipGap) {
                    label
                    if menu && remove == nil { Image(systemName: "chevron.down").font(.system(size: T.chipGlyph * 0.8, weight: .bold)).opacity(0.7) }
                }
                .padding(.leading, T.chipPad)
                .padding(.trailing, remove == nil ? T.chipPad : T.chipGap)
                .frame(height: T.chipH)
                .contentShape(Capsule())
            }
            .buttonStyle(PressStyle())
            if let remove {
                Button(action: remove) {
                    Image(systemName: "xmark").font(.system(size: T.chipGlyph * 0.8, weight: .bold))
                        .padding(.leading, T.chipGap)
                        .padding(.trailing, T.chipPad - T.chipGap)
                        .frame(height: T.chipH)
                        .contentShape(Capsule())
                }
                .buttonStyle(ChipRemoveStyle())
                .help(removeLabel)
                .accessibilityLabel(removeLabel)
            }
        }
        .font(.navia(T.textSM, .medium))
        .lineLimit(1)
        .fixedSize()
        .foregroundStyle((on ? T.onChip : T.t1).color)
        .background {
            if on {
                Capsule().fill(T.chipOn.color)
            } else {
                Capsule().fill((hovering ? T.glassHover : T.glass).color)
                    .overlay(Capsule().strokeBorder(T.glassEdge.color, lineWidth: 1))
                    .animation(T.hover(hovering), value: hovering)
            }
        }
        .onHover { hovering = $0 }
    }
}

private struct ChipRemoveStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        ChipRemoveBody(label: configuration.label).modifier(PressShrink(pressed: configuration.isPressed))
    }
}

private struct ChipRemoveBody<Label: View>: View {
    let label: Label
    @State private var hovering = false
    var body: some View { label.opacity(hovering ? 1 : 0.7).onHover { hovering = $0 } }
}

/// A number field, such as Year or BPM. It shows the focus ring while typing and the invalid ring when its value
/// is the problem.
struct NumberField: View {
    let placeholder: String
    @Binding var text: String
    let bad: Bool
    let accent: RGBA
    @FocusState private var focused: Bool

    var body: some View {
        TextField("", text: $text, prompt: Text(placeholder).foregroundStyle(T.t3.color))
            .textFieldStyle(.plain)
            .font(.navia(T.textSM).monospacedDigit())
            .focused($focused)
            .padding(.horizontal, T.chipPad)
            .frame(width: T.numberFieldW, height: T.chipH)
            .background(Capsule().fill(T.row.color))
            .focusRing(focused, accent)
            .invalidRing(bad && !focused)
            .accessibilityLabel(placeholder)
    }
}

/// A floating panel anchored under its trigger, or above when there's more room, kept inside the window.
/// A click outside closes it. `wanted` is the content's natural height, capped at `T.popMaxH`.
struct AnchoredPopover<Content: View>: View {
    let anchor: CGRect
    let width: CGFloat
    let wanted: CGFloat
    let windowSize: CGSize
    /// The least it shrinks to when the window is short.
    var minHeight: CGFloat = T.px(120)
    let close: () -> Void
    @ViewBuilder var content: Content
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        let edge = T.px(10), gap = T.px(6), w = min(width, windowSize.width - edge * 2)
        let capped = min(wanted, T.popMaxH)
        let below = windowSize.height - anchor.maxY - gap - edge
        let above = anchor.minY - gap - edge
        let down = below >= capped || below >= above
        let height = max(minHeight, min(capped, down ? below : above))
        let x = max(edge, min(anchor.minX, windowSize.width - w - edge))
        let y = down ? anchor.maxY + gap : max(edge, anchor.minY - gap - height)
        return ZStack(alignment: .topLeading) {
            Color.clear
                .contentShape(Rectangle())
                .onTapGesture { close() }
                .accessibilityHidden(true)
            content
                .frame(width: w, height: isSnapshot ? T.popMaxH : wanted == 0 ? nil : height, alignment: .top)
                .floatPanel(clip: true)
                .offset(x: x, y: y)
                .opacity(wanted == 0 && !isSnapshot ? 0 : 1)
        }
        .frame(width: windowSize.width, height: windowSize.height, alignment: .topLeading)
    }
}

extension EnvironmentValues {
    /// false while the window is hidden or being resized (set by `RootView`).
    @Entry var marqueeMoves = true
}

extension View {
    /// text cut short by its line limit shows `text` in full as its tooltip, only while it's cut (credits pass their joined list).
    func fullTextHelp(_ text: String) -> some View { modifier(FullTextHelp(text: text)) }
}

private struct FullTextHelp: ViewModifier {
    let text: String
    @State private var shown: CGSize = .zero
    @State private var ideal: CGSize = .zero

    func body(content: Content) -> some View {
        content
            .onGeometryChange(for: CGSize.self, of: { $0.size }) { shown = $0 }
            .background { content.fixedSize().hidden().onGeometryChange(for: CGSize.self, of: { $0.size }) { ideal = $0 } }
            .help(cut ? text : "")
    }

    /// one line's full width past the room of the lines shown. A clamp wraps at words, so a few cut clamps read as fitting, never the reverse.
    private var cut: Bool {
        guard ideal.height > 0, shown.width > 0 else { return false }
        let lines = max(1, (shown.height / ideal.height).rounded())
        return ideal.width > shown.width * lines + 0.5
    }
}

extension NSTextField {
    /// AppKit's `fullTextHelp`: the full string as the tooltip while the field's line is cut.
    func showFullTextWhenCut() {
        toolTip = !isHidden && (cell?.cellSize.width ?? 0) > bounds.width + 0.5 ? stringValue : nil
    }
}

/// A player line that scrolls sideways when it doesn't fit; the line itself holds the layout.
struct MarqueeText<Content: View>: View {
    @ViewBuilder let content: Content
    @Environment(\.marqueeMoves) private var moves
    @Environment(\.appearsActive) private var active
    @State private var contentW: CGFloat = 0
    @State private var boxW: CGFloat = 0

    var body: some View {
        let overflows = contentW > boxW + 0.5 && boxW > 0
        let runs = overflows && moves && active && !Motion.isReduced
        content
            .lineLimit(1)
            // still takes clicks at its resting place, where opacity 0 wouldn't.
            .opacity(runs ? T.marqueeRestOpacity : overflows ? 0 : 1)
            .overlay(alignment: .leading) {
                if overflows && !runs {
                    // at rest a line that doesn't fit fades out at its end, instead of each linked piece ending in "…".
                    content.lineLimit(1).fixedSize(horizontal: true, vertical: false)
                        .frame(width: boxW, alignment: .leading)
                        .clipped()
                        .mask(LinearGradient(stops: [.init(color: .black, location: 0),
                                                     .init(color: .black, location: 1 - T.marqueeFade / boxW),
                                                     .init(color: .clear, location: 1)],
                                             startPoint: .leading, endPoint: .trailing))
                }
                if runs {
                    MarqueeRoll(strip: AnyView(HStack(spacing: T.marqueeGap) { content; content }.lineLimit(1).fixedSize()),
                                distance: contentW + T.marqueeGap, stripWidth: contentW * 2 + T.marqueeGap)
                        .frame(width: boxW)
                        .allowsHitTesting(false)
                        .accessibilityHidden(true)
                }
            }
            .background { content.lineLimit(1).fixedSize(horizontal: true, vertical: false).hidden().onGeometryChange(for: CGFloat.self, of: { $0.size.width }) { contentW = $0 } }
            .onGeometryChange(for: CGFloat.self, of: { $0.size.width }) { boxW = $0 }
    }
}

/// moved by a Core Animation keyframe animation, so the window server runs the roll and the app draws nothing.
private struct MarqueeRoll: NSViewRepresentable {
    let strip: AnyView
    let distance: CGFloat
    let stripWidth: CGFloat

    func makeNSView(context: Context) -> MarqueeRollView { MarqueeRollView() }

    func updateNSView(_ view: MarqueeRollView, context: Context) {
        view.host.rootView = AnyView(strip.environment(\.self, context.environment))
        view.distance = distance
        view.stripWidth = stripWidth
        view.needsLayout = true
    }
}

/// one clock for every marquee, each cycle as long as the longest roll, so a title and the line under it never drift.
@MainActor final class MarqueeSync {
    static let shared = MarqueeSync()
    private let views = NSHashTable<MarqueeRollView>.weakObjects()

    var cycle: Double { T.marqueePause + (views.allObjects.map(\.roll).max() ?? 0) }

    func join(_ view: MarqueeRollView) { views.add(view); restart() }
    func leave(_ view: MarqueeRollView) { views.remove(view); restart() }

    private func restart() {
        let epoch = CACurrentMediaTime(), cycle = cycle
        for view in views.allObjects { view.start(epoch: epoch, cycle: cycle) }
    }
}

final class MarqueeRollView: NSView {
    let host = NSHostingView(rootView: AnyView(EmptyView()))
    private let fade = CAGradientLayer()
    var distance: CGFloat = 0
    /// both copies and the gap; set by the caller, since a host with no sizing options reports no fitting size.
    var stripWidth: CGFloat = 0
    private var started: (distance: CGFloat, width: CGFloat)?
    var roll: Double { Double(distance / T.marqueeSpeed) }

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.masksToBounds = true
        fade.startPoint = CGPoint(x: 0, y: 0.5)
        fade.endPoint = CGPoint(x: 1, y: 0.5)
        layer?.mask = fade
        host.sizingOptions = []
        host.wantsLayer = true
        addSubview(host)
    }

    required init?(coder: NSCoder) { nil }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func layout() {
        super.layout()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        host.frame = CGRect(x: 0, y: 0, width: stripWidth, height: bounds.height)
        fade.frame = bounds
        CATransaction.commit()
        guard distance > 0, bounds.width > 0, started.map({ $0 != (distance, bounds.width) }) ?? true else { return }
        started = (distance, bounds.width)
        MarqueeSync.shared.join(self)
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window == nil { started = nil; MarqueeSync.shared.leave(self) }
    }

    /// rests, rolls, then holds where its second copy looks like the start until the shared cycle ends.
    func start(epoch: CFTimeInterval, cycle total: Double) {
        guard distance > 0, bounds.width > 0, total > 0 else { return }
        let begin = NSNumber(value: T.marqueePause / total)
        let end = NSNumber(value: min(1, (T.marqueePause + roll) / total))
        let move = CAKeyframeAnimation(keyPath: "transform.translation.x")
        move.values = [0, 0, -distance, -distance]
        move.keyTimes = [0, begin, end, 1]

        let edge = NSNumber(value: Double(T.marqueeFade / bounds.width))
        fade.locations = [0, edge, NSNumber(value: 1 - edge.doubleValue), 1]
        let black = NSColor.black.cgColor, clear = NSColor.clear.cgColor
        let resting = [black, black, black, clear], rolling = [clear, black, black, clear]
        fade.colors = resting
        let edges = CAKeyframeAnimation(keyPath: "colors")
        let fadeIn = T.marqueeFadeIn / total
        edges.values = [resting, resting, rolling, rolling, resting, resting]
        edges.keyTimes = [0, begin, NSNumber(value: min(end.doubleValue, begin.doubleValue + fadeIn)), end,
                          NSNumber(value: min(1, end.doubleValue + fadeIn)), 1]

        for (animation, target) in [(move, host.layer), (edges, fade)] {
            guard let target else { continue }
            animation.duration = total
            animation.repeatCount = .infinity
            animation.isRemovedOnCompletion = false
            animation.beginTime = target.convertTime(epoch, from: nil)
            target.add(animation, forKey: "marquee")
        }
    }
}

/// the dark veil and glyph over artwork while it's hovered. the caller clips it to the art's corners.
struct ArtOverlay<Glyph: View>: View {
    let shown: Bool
    @ViewBuilder let glyph: Glyph

    var body: some View {
        ZStack {
            T.windowButtonOnArt.color
            glyph.foregroundStyle(T.tOnArt.color).shadow(T.shadowKnob)
        }
        .opacity(shown ? 1 : 0)
        .animation(T.hover(shown), value: shown)
        .allowsHitTesting(false)
    }
}

/// as the system's search fields do it: the ✕ shows only while there's text, and emptying the field keeps the keyboard in it.
struct SearchClearButton: View {
    @Binding var text: String
    var size: CGFloat = T.searchClear
    @State private var hovering = false

    var body: some View {
        if !text.isEmpty {
            Button { text = "" } label: {
                Image(systemName: "xmark.circle.fill").font(.system(size: size)).foregroundStyle((hovering ? T.t1 : T.t3).color)
                    .animation(T.hover(hovering), value: hovering)
            }
            .buttonStyle(.plain)
            .focusable(false)
            .onHover { hovering = $0 }
            .help(L("Clear Search"))
            .accessibilityLabel(L("Clear Search"))
        }
    }
}

extension View {
    /// escape empties a search field that has text, as the system's search fields do.
    func escapeClears(_ text: Binding<String>) -> some View {
        onKeyPress(.escape) {
            guard !text.wrappedValue.isEmpty else { return .ignored }
            text.wrappedValue = ""
            return .handled
        }
    }
}

extension View {
    /// for a text field in the top bar: the window server moves the window on a press and drag anywhere in the title
    /// bar that no title bar view blocks, before the field sees it, so text couldn't be selected.
    func holdsWindowStill() -> some View { background(TitlebarDragBlock().allowsHitTesting(false)) }
}

/// the drag region AppKit hands the window server counts only the title bar's own views, not content under the bar, so
/// a twin of this view's frame goes into the title bar view.
private struct TitlebarDragBlock: NSViewRepresentable {
    func makeNSView(context: Context) -> Marker { Marker() }
    func updateNSView(_ nsView: Marker, context: Context) { nsView.place() }

    final class Marker: NSView {
        private let block = Block()
        private var observers: [NSObjectProtocol] = []

        isolated deinit {
            observers.forEach(NotificationCenter.default.removeObserver)
            block.removeFromSuperview()
        }

        override func hitTest(_ point: NSPoint) -> NSView? { nil }
        override func layout() { super.layout(); place() }
        override func viewDidHide() { super.viewDidHide(); place() }
        override func viewDidUnhide() { super.viewDidUnhide(); place() }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            observers.forEach(NotificationCenter.default.removeObserver)
            observers = []
            place()
            guard let window else { return }
            // SwiftUI moves the hosts this view sits in, not this view, and a page scrolls by its clip view's bounds.
            var ancestor = superview
            while let view = ancestor, view !== window.contentView {
                var names = [NSView.frameDidChangeNotification]
                if view is NSClipView {
                    view.postsBoundsChangedNotifications = true
                    names.append(NSView.boundsDidChangeNotification)
                }
                for name in names {
                    observers.append(NotificationCenter.default.addObserver(forName: name, object: view, queue: .main) { [weak self] _ in
                        MainActor.assumeIsolated { self?.place() }
                    })
                }
                ancestor = view.superview
            }
            for name in [NSWindow.didResizeNotification, NSWindow.didExitFullScreenNotification] {
                observers.append(NotificationCenter.default.addObserver(forName: name, object: window, queue: .main) { [weak self] _ in
                    MainActor.assumeIsolated { self?.place() }
                })
            }
        }

        func place() {
            guard let window, !isHiddenOrHasHiddenAncestor, !visibleRect.isEmpty, let bar = window.standardWindowButton(.closeButton)?.superview else {
                block.removeFromSuperview()
                return
            }
            if block.superview !== bar { bar.addSubview(block) }
            block.frame = bar.convert(visibleRect, from: self)
        }
    }

    final class Block: NSView {
        override var mouseDownCanMoveWindow: Bool { false }
        override func hitTest(_ point: NSPoint) -> NSView? { nil }
    }
}
