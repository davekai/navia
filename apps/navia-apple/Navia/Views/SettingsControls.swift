import AppKit
import SwiftUI

extension EnvironmentValues {
    /// the Settings search's words; empty shows every row.
    @Entry var settingsQuery: [String] = []
}

/// the lowercased runs of letters and digits.
func settingsSearchWords(_ text: String) -> [String] {
    text.lowercased().split { !($0.isLetter || $0.isNumber) }.map(String.init)
}

/// every word starts a word of `texts`.
func settingsMatch(_ words: [String], _ texts: String?...) -> Bool {
    let hay = settingsSearchWords(texts.compactMap { $0 }.joined(separator: " "))
    return words.allSatisfy { word in hay.contains { $0.hasPrefix(word) } }
}

/// hidden when the search left none of its rows.
struct SettingsSection<Content: View>: View {
    let title: String
    @ViewBuilder let content: Content

    var body: some View {
        Group(subviews: content) { rows in
            if !rows.isEmpty {
                VStack(alignment: .leading, spacing: T.px(10)) {
                    Text(title).font(.navia(T.textSection, .semibold)).tracking(-0.012 * T.textSection)
                    SettingsCard { rows }
                }
            }
        }
    }
}

/// A group of rows on the panel glass, so the window's translucency shows through like in the sidebar.
struct SettingsCard<Content: View>: View {
    @ViewBuilder let content: Content
    var body: some View {
        Group(subviews: content) { rows in
            VStack(spacing: 0) {
                ForEach(Array(rows.enumerated()), id: \.element.id) { index, row in
                    row.overlay(alignment: .bottom) { if index < rows.count - 1 { Rectangle().fill(T.line.color).frame(height: 1) } }
                }
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: T.px(14), style: .continuous))
        .panelPlain(radius: T.px(14))
    }
}

/// One row of a settings card, with a label, an optional hint and its control.
/// `dim` marks a row whose control has no backend yet, and the hint says why.
struct SettingsRow<Control: View>: View {
    let label: String
    var hint: String?
    var dim = false
    @ViewBuilder let control: Control
    @Environment(\.settingsQuery) private var query

    var body: some View {
        if settingsMatch(query, label, hint) {
            let gap = T.px(24)
            SettingsRowLayout(gap: gap, stackGap: T.px(8), textMin: T.settingsTextMin) {
                text
                HStack(spacing: gap) { control }
            }
            .padding(.vertical, T.px(10))
            .padding(.horizontal, T.px(16))
            .frame(maxWidth: .infinity, minHeight: T.px(60), alignment: .leading)
        }
    }

    private var text: some View {
        VStack(alignment: .leading, spacing: T.px(1)) {
            Text(label).font(.navia(T.textMD, .medium)).foregroundStyle((dim ? T.t2 : T.t1).color)
            if let hint {
                Text(hint).font(.navia(T.textSM)).foregroundStyle(T.t2.color).fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// the text and the control side by side while the text keeps `textMin`; narrower, the control goes under the text.
private struct SettingsRowLayout: Layout {
    let gap: CGFloat
    let stackGap: CGFloat
    let textMin: CGFloat

    private func beside(_ width: CGFloat, _ control: CGSize) -> Bool { width - control.width - gap >= textMin }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let control = subviews[1].sizeThatFits(.unspecified)
        guard let width = proposal.width, width.isFinite else {
            let text = subviews[0].sizeThatFits(.unspecified)
            return CGSize(width: text.width + gap + control.width, height: max(text.height, control.height))
        }
        if beside(width, control) {
            let text = subviews[0].sizeThatFits(ProposedViewSize(width: width - control.width - gap, height: nil))
            return CGSize(width: width, height: max(text.height, control.height))
        }
        let text = subviews[0].sizeThatFits(ProposedViewSize(width: width, height: nil))
        let below = subviews[1].sizeThatFits(ProposedViewSize(width: min(control.width, width), height: nil))
        return CGSize(width: width, height: text.height + stackGap + below.height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let control = subviews[1].sizeThatFits(.unspecified)
        if beside(bounds.width, control) {
            let textWidth = bounds.width - control.width - gap
            subviews[0].place(at: CGPoint(x: bounds.minX, y: bounds.midY), anchor: .leading, proposal: ProposedViewSize(width: textWidth, height: nil))
            subviews[1].place(at: CGPoint(x: bounds.maxX, y: bounds.midY), anchor: .trailing, proposal: ProposedViewSize(control))
        } else {
            let text = subviews[0].sizeThatFits(ProposedViewSize(width: bounds.width, height: nil))
            subviews[0].place(at: bounds.origin, proposal: ProposedViewSize(width: bounds.width, height: nil))
            subviews[1].place(at: CGPoint(x: bounds.minX, y: bounds.minY + text.height + stackGap), proposal: ProposedViewSize(width: min(control.width, bounds.width), height: nil))
        }
    }
}

/// opens a sub-page. `terms` are that page's own settings, so the search finds them here; the matched ones replace the hint.
struct SettingsLinkRow: View {
    let label: String
    let hint: String
    /// Shown before the chevron, like a size.
    var value: String? = nil
    var terms: [String] = []
    let action: () -> Void
    @State private var hovering = false
    @Environment(\.settingsQuery) private var query

    var body: some View {
        if let shown = shownHint {
            Button(action: action) {
                HStack(spacing: T.px(24)) {
                    VStack(alignment: .leading, spacing: T.px(1)) {
                        Text(label).font(.navia(T.textMD, .medium)).foregroundStyle(T.t1.color)
                        Text(shown).font(.navia(T.textSM)).foregroundStyle(T.t2.color)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    if let value { Text(value).font(.navia(T.textMD)).foregroundStyle(T.t2.color).fixedSize() }
                    Image(systemName: "chevron.right").font(.system(size: T.px(14), weight: .bold)).foregroundStyle(T.t3.color)
                }
                .padding(.vertical, T.px(10))
                .padding(.horizontal, T.px(16))
                .frame(maxWidth: .infinity, minHeight: T.px(60))
                .background(hovering ? T.hover.color : .clear)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .onHover { hovering = $0 }
        }
    }

    private var shownHint: String? {
        if settingsMatch(query, label, hint) { return hint }
        let found = terms.filter { settingsMatch(query, label, $0) }
        return found.isEmpty ? nil : found.joined(separator: ", ")
    }
}

/// A row control with no backend yet, dimmed, with the current value and a caret.
struct DisabledPill: View {
    let value: String
    var width: CGFloat? = T.px(240)

    var body: some View {
        HStack(spacing: T.px(6)) {
            Text(value).font(.navia(T.textMD)).lineLimit(1).frame(maxWidth: .infinity, alignment: .leading)
            Image(systemName: "chevron.up.chevron.down").font(.system(size: T.px(11), weight: .medium)).foregroundStyle(T.t2.color)
        }
        .padding(.leading, T.px(16))
        .padding(.trailing, T.px(12))
        .frame(width: width, height: T.controlH)
        .glassPill()
        .opacity(T.disabledOpacity)
    }
}

/// The native segmented control, at Settings' size or the larger size used in page toolbars.
struct SegmentedChoice<Value: Hashable>: View {
    struct Option { let value: Value; let label: String }

    /// `settings` for Settings' rows, `glass` for page toolbars.
    enum Look { case settings, glass }
    let look: Look
    let value: Value
    let options: [Option]
    let accent: RGBA
    let label: String
    let onChange: (Value) -> Void

    var body: some View {
        Picker(label, selection: Binding(get: { value }, set: { if $0 != value { onChange($0) } })) {
            ForEach(options.indices, id: \.self) { Text(options[$0].label).tag(options[$0].value) }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .controlSize(look == .glass ? .large : .regular)
        .tint(accent.color)
        .fixedSize()
    }
}

struct PlainSlider: View {
    enum Look: Equatable {
        /// The native slider, tinted with the accent.
        case native(accent: RGBA)
        /// The fill grows from `origin` in the accent color.
        case fill(origin: Double?, accent: RGBA)
        case hue
    }

    let label: String
    let value: Double
    let min: Double
    let max: Double
    var step: Double = 1
    var look: Look
    var width: CGFloat? = T.px(200)
    var disabled = false
    var valueText: String? = nil
    let onChange: (Double) -> Void
    var onEnd: () -> Void = {}

    var body: some View {
        if case .native(let accent) = look {
            Slider(value: Binding(get: { value }, set: { next in
                let stepped = step > 0 ? (next / step).rounded() * step : next
                onChange(Swift.min(max, Swift.max(min, stepped)))
            }), in: min...max, onEditingChanged: { editing in if !editing { onEnd() } })
            .labelsHidden()
            .tint(accent.color)
            .disabled(disabled)
            .frame(width: width)
            .accessibilityLabel(label)
            .accessibilityValue(valueText ?? String(format: "%g", value))
        } else {
            drawn
        }
    }

    private var drawn: some View {
        GeometryReader { geo in
            let thumb = T.px(14)
            let usable = Swift.max(1, geo.size.width - thumb)
            let fraction = Swift.min(1, Swift.max(0, (value - min) / (max - min)))
            ZStack(alignment: .leading) {
                track(usable: usable, thumb: thumb)
                Circle().fill(look == .hue ? T.knob.color : T.t1.color)
                    .frame(width: thumb, height: thumb)
                    .shadow(look == .hue ? T.shadowKnobHue : T.shadowKnob)
                    .offset(x: fraction * usable)
            }
            .frame(maxHeight: .infinity)
            .contentShape(Rectangle())
            .gesture(DragGesture(minimumDistance: 0)
                .onChanged { drag in
                    guard !disabled else { return }
                    let f = Swift.min(1, Swift.max(0, (drag.location.x - thumb / 2) / usable))
                    var next = min + f * (max - min)
                    if step > 0 { next = (next / step).rounded() * step }
                    onChange(Swift.min(max, Swift.max(min, next)))
                }
                .onEnded { _ in onEnd() })
        }
        .frame(width: width, height: T.px(16))
        .opacity(disabled ? T.disabledOpacity : 1)
        .accessibilityElement()
        .accessibilityLabel(label)
        .accessibilityValue(valueText ?? String(format: "%g", value))
        .accessibilityAdjustableAction { direction in
            guard !disabled else { return }
            let delta = step > 0 ? step : (max - min) / 20
            onChange(Swift.min(max, Swift.max(min, value + (direction == .increment ? delta : -delta))))
            onEnd()
        }
    }

    @ViewBuilder private func track(usable: CGFloat, thumb: CGFloat) -> some View {
        switch look {
        case .hue:
            Capsule()
                .fill(LinearGradient(colors: ["ff6666", "ffcc66", "99ff66", "66ffcc", "66ccff", "9966ff", "ff66cc", "ff6666"].compactMap { RGBA(hex: $0)?.color }, startPoint: .leading, endPoint: .trailing))
                .frame(height: T.px(6))
        case .native:
            EmptyView()
        case .fill(let origin, let accent):
            let pct: (Double) -> CGFloat = { v in thumb / 2 + CGFloat(Swift.min(1, Swift.max(0, (v - min) / (max - min)))) * usable }
            ZStack(alignment: .leading) {
                Capsule().fill(T.panel2.color).frame(height: T.px(4))
                let (from, to) = (pct(Swift.min(value, origin ?? min)), pct(Swift.max(value, origin ?? min)))
                Capsule().fill(accent.color).frame(width: Swift.max(0, to - from), height: T.px(4)).offset(x: from)
            }
        }
    }
}

/// A plain text field in a pill, for the accent hex, preset names and rule values.
struct PillTextField: View {
    let placeholder: String
    @Binding var text: String
    let accent: RGBA
    var width: CGFloat? = nil
    var height: CGFloat = T.controlH
    var invalid = false
    var font: Font = .navia(T.textMD)
    var onSubmit: () -> Void = {}
    var onEscape: () -> Void = {}
    /// Called when the field loses focus, for a value that applies once it's entered.
    var onBlur: () -> Void = {}
    var filled = false
    /// Takes focus when it appears, after AppKit's own launch focus has settled.
    var autoFocus = false
    /// Room kept at the right end for an action laid over it.
    var trailingRoom: CGFloat = 0
    @FocusState private var focused: Bool

    var body: some View {
        TextField("", text: $text, prompt: Text(placeholder).foregroundStyle(T.t3.color))
            .textFieldStyle(.plain)
            .font(font)
            .focused($focused)
            .onSubmit(onSubmit)
            .onKeyPress(.escape) { onEscape(); return .ignored }
            .onChange(of: focused) { _, now in if !now { onBlur() } }
            .padding(.horizontal, filled ? T.px(12) : T.fieldPad)
            .padding(.trailing, trailingRoom)
            .frame(width: width, height: height)
            .frame(maxWidth: width == nil ? .infinity : nil)
            .modifier(FieldBackground(filled: filled))
            .focusRing(focused || invalid, ring, width: invalid ? T.invalidRingW : T.focusRingWidth)
            .task {
                guard autoFocus else { return }
                try? await Task.sleep(for: .milliseconds(150))
                focused = true
            }
    }

    /// The invalid ring wins over focus.
    private var ring: RGBA { invalid ? T.invalidRing : accent }
}

private struct FieldBackground: ViewModifier {
    let filled: Bool
    func body(content: Content) -> some View {
        if filled { content.background(Capsule().fill(T.panel2.color)) } else { content.glassPill() }
    }
}

struct AccentPicker: View {
    let theme: Theme
    @Environment(\.pageContext) private var ctx
    @State private var text = ""

    var body: some View {
        let wrap = ctx.cssWidth <= 760
        let layout = wrap ? AnyLayout(FlowLayoutWrap(spacing: T.px(12))) : AnyLayout(HStackLayout(spacing: T.px(12)))
        layout {
            HStack(spacing: T.px(7)) {
                systemSwatch
                ForEach(accentPresets, id: \.self) { swatch($0) }
            }
            PlainSlider(label: L("Accent hue"), value: theme.hue, min: 0, max: 359, look: .hue, width: T.px(96),
                        onChange: { theme.setAccent(hue: $0) })
            PillTextField(placeholder: "", text: Binding(get: { text }, set: { typed($0) }), accent: theme.accent, width: T.px(84), height: T.px(34),
                          invalid: !isHex(text), font: .navia(T.textSM), filled: true)
        }
        .onAppear { text = theme.hex }
        .onChange(of: theme.hex) { _, new in if !isHex(text) || text.lowercased() != new { text = new } }
    }

    private func isHex(_ value: String) -> Bool { value.hasPrefix("#") && value.count == 7 && RGBA(hex: value) != nil }

    private func typed(_ value: String) {
        text = String(value.prefix(7))
        if isHex(text) { theme.setAccent(hex: text.lowercased()) }
    }

    private func swatch(_ hex: String) -> some View {
        let on = !theme.followsSystem && theme.hex == "#\(hex)"
        return Button { theme.setAccent(hex: "#\(hex)") } label: {
            ZStack {
                Circle().fill(RGBA(hex: hex)!.color)
                Circle().strokeBorder(T.glassEdge.color, lineWidth: 1)
                if on { check.foregroundStyle(theme.onAccent.color) }
            }
            .selectedSwatch(on)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(L("Use #{hex}", ["hex": hex]))
    }

    /// `--accent-wheel`: every hue at the accent's saturation and lightness, red at the top.
    private var systemSwatch: some View {
        let on = theme.followsSystem
        let hues = stride(from: 0.0, through: 360, by: 30).map { RGBA.hsl($0, T.accentS, T.accentL).color }
        return Button { theme.followSystem() } label: {
            ZStack {
                Circle().fill(AngularGradient(colors: hues, center: .center, startAngle: .degrees(-90), endAngle: .degrees(270)))
                Circle().strokeBorder(T.glassEdge.color, lineWidth: 1)
                if on { check.foregroundStyle(T.t1.color).shadow(color: T.accentWheelCheckShadow.color, radius: T.accentWheelCheckBlur / 2) }
            }
            .selectedSwatch(on)
        }
        .buttonStyle(.plain)
        .help(L("System"))
        .accessibilityLabel(L("System"))
    }

    private var check: some View { Image(systemName: "checkmark").font(.system(size: T.px(12), weight: .bold)) }
}

private extension View {
    func selectedSwatch(_ on: Bool) -> some View {
        frame(width: T.accentSwatch, height: T.accentSwatch)
            .overlay(Circle().strokeBorder(T.t1.color, lineWidth: on ? 2 : 0).padding(on ? T.px(-4) : 0))
            .contentShape(Circle())
    }
}

/// Wraps children to new lines, for the accent picker on narrow windows.
struct FlowLayoutWrap: Layout {
    var spacing: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        arrange(subviews, width: proposal.width ?? .infinity).size
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        for (index, origin) in arrange(subviews, width: bounds.width).origins.enumerated() {
            subviews[index].place(at: CGPoint(x: bounds.minX + origin.x, y: bounds.minY + origin.y), proposal: .unspecified)
        }
    }

    private func arrange(_ subviews: Subviews, width: CGFloat) -> (origins: [CGPoint], size: CGSize) {
        var x: CGFloat = 0, y: CGFloat = 0, rowH: CGFloat = 0, right: CGFloat = 0
        var origins: [CGPoint] = []
        for sub in subviews {
            let size = sub.sizeThatFits(.unspecified)
            if x > 0 && x + size.width > width { x = 0; y += rowH + spacing; rowH = 0 }
            origins.append(CGPoint(x: x, y: y))
            x += size.width + spacing
            right = max(right, x - spacing)
            rowH = max(rowH, size.height)
        }
        return (origins, CGSize(width: right, height: y + rowH))
    }
}
