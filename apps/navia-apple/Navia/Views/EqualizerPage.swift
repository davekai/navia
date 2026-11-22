import AppKit
import SwiftUI
import UniformTypeIdentifiers

private func db(_ value: Double) -> String { "\(formatDb(value)) dB" }

/// The Equalizer page. Off by default, and dimmed and inert when off.
struct EqualizerPage: View {
    let shell: ShellModel
    @State private var activeFilter: UInt64?
    @State private var naming = false
    @State private var presetName = ""
    @State private var importError: String?
    @Environment(\.pageContext) private var ctx

    private var store: EqualizerStore { shell.equalizer }
    private var state: EqState { store.state }
    private var profile: EqProfile { state.current }
    private var parametric: Bool { profile.mode == .parametric }
    private var theme: Theme { shell.theme }
    private var sampleMode: Bool { CoreMode.useSample }

    var body: some View {
        PageFrame(shell: shell, nav: .none, title: L("Equalizer")) {
            PageScroll { content }
        }
        .onAppear { shell.loadOutputs() }
    }

    private var content: some View {
        VStack(spacing: T.px(22)) {
            SettingsCard {
                SettingsRow(label: L("Equalizer"), hint: masterHint) {
                    ToggleSwitch(isOn: Binding(get: { state.enabled }, set: { on in
                        store.updateAndSave { $0.enabled = on }
                        shell.applyEqualizer()
                    }), accent: theme.accent, label: L("Equalizer"))
                }
                SettingsRow(label: L("Output"), hint: L("Each output remembers its own settings on this page.")) {
                    GlassSelect(shell: shell, label: L("Output"), value: state.output, options: outputOptions, width: T.px(210)) { value in
                        store.updateAndSave { $0.output = value }
                        shell.applyEqualizer()
                    }
                }
            }
            VStack(spacing: T.px(30)) {
                graphCard
                section(L("Gain")) {
                    SettingsRow(label: L("Preamp"), hint: L("{level} before the equalizer.", ["level": db(profile.preamp)])) {
                        PlainSlider(label: L("Preamp"), value: profile.preamp, min: -eqRange, max: eqRange, step: 0.5, look: fillLook(origin: 0),
                                    onChange: { v in edit { $0.preamp = v } }, onEnd: saveAndApply)
                    }
                    SettingsRow(label: L("Soft limiter"), hint: limiterHint) {
                        ToggleSwitch(isOn: Binding(get: { profile.limiter }, set: { v in editAndSave { $0.limiter = v } }), accent: theme.accent, label: L("Soft limiter"))
                    }
                }
                section(L("Headphone Correction")) {
                    SettingsRow(label: L("Headphone model"), hint: headphoneHint) { headphoneControls }
                }
                section(L("Stereo")) {
                    SettingsRow(label: L("Balance"), hint: eqBalanceText(profile.balance)) {
                        PlainSlider(label: L("Balance"), value: profile.balance, min: -100, max: 100, look: fillLook(origin: 0), valueText: eqBalanceText(profile.balance),
                                    onChange: { v in edit { $0.balance = abs(v) < 4 ? 0 : v } }, onEnd: saveAndApply)
                    }
                    SettingsRow(label: L("Mono"), hint: L("Plays both channels as one.")) {
                        ToggleSwitch(isOn: Binding(get: { profile.mono }, set: { v in editAndSave { $0.mono = v } }), accent: theme.accent, label: L("Mono"))
                    }
                    SettingsRow(label: L("Crossfeed"), hint: L("Blends a little of each channel into the other, for easier listening on headphones.")) {
                        ToggleSwitch(isOn: Binding(get: { profile.crossfeed }, set: { v in editAndSave { $0.crossfeed = v } }), accent: theme.accent, label: L("Crossfeed"))
                    }
                    if profile.crossfeed {
                        SettingsRow(label: L("Crossfeed strength"), hint: "\(Int(profile.crossfeedAmount.rounded()))%") {
                            PlainSlider(label: L("Crossfeed strength"), value: profile.crossfeedAmount, min: 0, max: 100, look: fillLook(origin: nil),
                                        onChange: { v in edit { $0.crossfeedAmount = v } }, onEnd: saveAndApply)
                        }
                    }
                }
            }
            .opacity(state.enabled ? 1 : 0.45)
            .disabled(!state.enabled)
            .allowsHitTesting(state.enabled)
        }
        .padding(.horizontal, T.gutter)
        .frame(maxWidth: T.px(1040))
        .frame(maxWidth: .infinity)
    }

    private func section<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        SettingsSection(title: title) { content() }
    }

    private var masterHint: String {
        state.enabled ? L("On. Your settings shape everything Navia plays.") : L("Off. Audio plays unprocessed: no EQ, correction or stereo changes. Your settings are kept.")
    }

    private var outputOptions: [SelectOption] {
        var list = [SelectOption(value: eqOutputDefault, label: L("System default"))]
        list += shell.outputOptions.map { SelectOption(value: $0.id, label: $0.name) }
        // A remembered output may be unplugged right now.
        if !list.contains(where: { $0.value == state.output }) { list.append(SelectOption(value: state.output, label: shell.outputLabel(state.output))) }
        return list
    }

    private var limiterHint: String {
        let peak = eqPeakLevel(profile, correction: state.correction(for: profile, sampleMode: sampleMode))
        return peak > 0.05 ? L("Catches peaks that would clip. These settings reach {gain}.", ["gain": db((peak * 10).rounded() / 10)]) : L("Catches peaks that would clip. These settings stay below 0 dB.")
    }

    private func fillLook(origin: Double?) -> PlainSlider.Look { .fill(origin: origin, accent: theme.accent) }

    // so a profile's first change creates it with its own filter ids.

    private func edit(_ change: (inout EqProfile) -> Void) {
        store.update { s in
            s.ensureProfile()
            var p = s.current
            change(&p)
            s.current = p
        }
        shell.applyEqualizer()
    }

    private func editAndSave(_ change: (inout EqProfile) -> Void) {
        edit(change)
        saveAndApply()
    }

    private func saveAndApply() {
        store.save()
        shell.applyEqualizer()
    }

    // MARK: Graph card

    private var graphCard: some View {
        let tint = Tint(hex: shell.player.current?.tintHex)
        let top = tint.vivid.mixed(with: T.panel, amount: 0.44)
        return VStack(alignment: .leading, spacing: T.px(14)) {
            toolbar
            if naming && nameTaken {
                Text(L("That name belongs to a built-in preset.")).font(.navia(T.textSM)).foregroundStyle(T.t2.color).frame(maxWidth: .infinity, alignment: .trailing)
            }
            EqGraphView(profile: profile, correction: state.correction(for: profile, sampleMode: sampleMode), foreground: tint.fg, accent: theme.accent, onAccent: theme.onAccent,
                        narrow: ctx.cssWidth <= 760, activeFilter: $activeFilter, onBand: setBand, onFilter: setFilter, onEnd: saveAndApply)
            if !profile.headphone.isEmpty {
                HStack(spacing: T.px(8)) {
                    Capsule().fill(T.t2.color).frame(width: T.px(22), height: 2).overlay(HStack(spacing: 3) { ForEach(0..<4, id: \.self) { _ in Rectangle().fill(T.panel.color).frame(width: 2, height: 3) } })
                    Text(L("Headphone correction for {name}", ["name": profile.headphone])).font(.navia(T.textSM)).foregroundStyle(T.t2.color)
                }
            }
            if parametric { filterList }
        }
        .padding(.horizontal, T.px(ctx.cssWidth <= 620 ? 12 : 20))
        .padding(.top, T.px(16))
        .padding(.bottom, T.px(18))
        .background(LinearGradient(stops: [.init(color: top.color, location: 0), .init(color: T.panel.color, location: 0.9)], startPoint: .top, endPoint: .bottom),
                    in: RoundedRectangle(cornerRadius: T.px(18), style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: T.px(18), style: .continuous).strokeBorder(T.line.color, lineWidth: 1).allowsHitTesting(false))
    }

    private var currentPresetName: String { parametric ? profile.parametricPreset : profile.preset }
    private var customPresets: [EqCustomPreset] { state.custom.filter { $0.mode == profile.mode } }
    private var selectedCustom: EqCustomPreset? { customPresets.first { $0.name == currentPresetName } }
    private var nameTaken: Bool {
        let trimmed = presetName.trimmingCharacters(in: .whitespaces).lowercased()
        return eqBuiltInPresets.contains { $0.name.lowercased() == trimmed }
    }

    private var toolbar: some View {
        let controls = Group {
            SegmentedChoice(look: .glass, value: profile.mode, options: [.init(value: .graphic, label: L("Graphic")), .init(value: .parametric, label: L("Parametric"))],
                            accent: theme.accent, label: L("Equalizer mode")) { mode in editAndSave { $0.mode = mode } }
            Spacer(minLength: T.px(16))
            if naming { savingControls } else { presetControls }
        }
        return Group {
            if ctx.cssWidth <= 620 {
                VStack(alignment: .leading, spacing: T.px(10)) { HStack(spacing: T.px(10)) { controls } }
            } else {
                HStack(spacing: T.px(10)) { controls }
            }
        }
    }

    private var presetOptions: [SelectOption] {
        (parametric ? [] : eqBuiltInPresets.map { SelectOption(value: $0.name, label: L($0.name), group: L("Built-in")) })
            + customPresets.map { SelectOption(value: $0.name, label: $0.name, group: L("Your presets")) }
    }

    @ViewBuilder private var presetControls: some View {
        HStack(spacing: T.px(8)) {
            if !presetOptions.isEmpty {
                GlassSelect(shell: shell, label: L("Preset"), placeholder: parametric ? L("Custom filters") : L("Custom curve"), value: currentPresetName, options: presetOptions, width: T.px(210)) { applyPreset($0) }
            }
            if let custom = selectedCustom {
                roundButton(symbol: "trash", label: L("Delete {name}", ["name": custom.name])) {
                    store.updateAndSave { s in
                        s.custom.removeAll { $0.name == custom.name && $0.mode == custom.mode }
                        for key in s.profiles.keys {
                            if custom.mode == .graphic, s.profiles[key]?.preset == custom.name { s.profiles[key]?.preset = "" }
                            if custom.mode == .parametric, s.profiles[key]?.parametricPreset == custom.name { s.profiles[key]?.parametricPreset = "" }
                        }
                    }
                }
            }
            roundButton(symbol: "square.and.arrow.down", label: L("Save as preset")) {
                presetName = selectedCustom?.name ?? ""
                naming = true
            }
            roundButton(symbol: "arrow.counterclockwise", label: parametric ? L("Set every filter to 0 dB") : L("Reset to Flat")) {
                if parametric {
                    editAndSave { p in p.filters = p.filters.map { var f = $0; f.gain = 0; return f }; p.parametricPreset = "" }
                } else {
                    editAndSave { $0.gains = [Double](repeating: 0, count: 10); $0.preset = N_("Flat") }
                }
            }
        }
    }

    private func roundButton(symbol: String, label: String, action: @escaping () -> Void) -> some View {
        IconButton(symbol: symbol, label: label, size: T.px(16), width: T.controlH, height: T.controlH, action: action).glassPill()
    }

    private var savingControls: some View {
        HStack(spacing: T.px(8)) {
            PillTextField(placeholder: L("Preset name"), text: Binding(get: { presetName }, set: { presetName = String($0.prefix(eqNameMax)) }), accent: theme.accent, width: T.px(200),
                          onSubmit: savePreset, onEscape: { naming = false })
            let blocked = presetName.trimmingCharacters(in: .whitespaces).isEmpty || nameTaken
            ActionButton(title: L("Save"), kind: .prominent(tint: theme.accent), enabled: !blocked, action: savePreset)
            ActionButton(title: L("Cancel")) { naming = false }
        }
    }

    private func savePreset() {
        let name = presetName.trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty, !nameTaken else { return }
        let entry = parametric ? EqCustomPreset(name: name, mode: .parametric, gains: nil, filters: profile.filters) : EqCustomPreset(name: name, mode: .graphic, gains: profile.gains, filters: nil)
        store.updateAndSave { s in
            s.custom.removeAll { $0.name == name && $0.mode == entry.mode }
            s.custom.append(entry)
            s.ensureProfile()
            var p = s.current
            if parametric { p.parametricPreset = name } else { p.preset = name }
            s.current = p
        }
        naming = false
    }

    private func applyPreset(_ value: String) {
        if parametric {
            if let preset = customPresets.first(where: { $0.name == value }), let filters = preset.filters {
                editAndSave { $0.filters = filters; $0.parametricPreset = value }
            }
            return
        }
        let gains = eqBuiltInPresets.first { $0.name == value }?.gains ?? customPresets.first { $0.name == value }?.gains
        if let gains { editAndSave { $0.gains = gains; $0.preset = value } }
    }

    private func setBand(_ band: Int, _ gain: Double) {
        edit { p in p.gains[band] = gain; p.preset = "" }
    }

    private func setFilter(_ index: Int, freq: Double?, gain: Double?, q: Double?) {
        edit { p in
            guard p.filters.indices.contains(index) else { return }
            if let freq { p.filters[index].freq = freq }
            if let gain { p.filters[index].gain = gain }
            if let q { p.filters[index].q = q }
            p.parametricPreset = ""
        }
    }

    // MARK: Parametric filters

    private var filterList: some View {
        VStack(alignment: .leading, spacing: T.px(8)) {
            ForEach(Array(profile.filters.enumerated()), id: \.element.id) { index, filter in
                filterRow(filter, index: index)
            }
            if profile.filters.count < eqMaxFilters {
                ActionButton(title: L("Add Filter"), symbol: "plus") {
                    var new: EqFilter?
                    store.update { s in
                        s.ensureProfile()
                        var p = s.current
                        let filter = s.newFilter(freq: eqEmptiestFreq(p.filters))
                        p.filters.append(filter)
                        p.parametricPreset = ""
                        s.current = p
                        new = filter
                    }
                    saveAndApply()
                    activeFilter = new?.id
                }
            }
        }
        .padding(.top, T.px(4))
    }

    private func filterRow(_ filter: EqFilter, index: Int) -> some View {
        let active = filter.id == activeFilter
        let narrow = ctx.cssWidth <= 620
        let params = Group {
            param(L("Frequency"), formatFreq(filter.freq)) {
                PlainSlider(label: L("Filter {index} frequency", ["index": String(index + 1)]), value: (freqToX(filter.freq) * 1000).rounded(), min: 0, max: 1000, look: fillLook(origin: nil), width: nil, valueText: formatFreq(filter.freq),
                            onChange: { v in activeFilter = filter.id; setFilter(index, freq: snapFreq(xToFreq(v / 1000)), gain: nil, q: nil) }, onEnd: saveAndApply)
            }
            param(L("Gain"), db(filter.gain)) {
                PlainSlider(label: L("Filter {index} gain", ["index": String(index + 1)]), value: filter.gain, min: -eqRange, max: eqRange, step: 0.5, look: fillLook(origin: 0), width: nil, valueText: db(filter.gain),
                            onChange: { v in activeFilter = filter.id; setFilter(index, freq: nil, gain: v, q: nil) }, onEnd: saveAndApply)
            }
            param("Q", fmtDec(filter.q, 1)) {
                PlainSlider(label: L("Filter {index} Q", ["index": String(index + 1)]), value: filter.q, min: 0.2, max: 8, step: 0.1, look: fillLook(origin: nil), width: nil, valueText: fmtDec(filter.q, 1),
                            onChange: { v in activeFilter = filter.id; setFilter(index, freq: nil, gain: nil, q: v) }, onEnd: saveAndApply)
            }
        }
        let canRemove = profile.filters.count > 1
        return HStack(alignment: narrow ? .top : .center, spacing: T.px(18)) {
            Text("\(index + 1)")
                .font(.navia(T.textXS, .semibold))
                .foregroundStyle((active ? theme.onAccent : T.t2).color)
                .frame(width: T.px(24), height: T.px(24))
                .background(Circle().fill((active ? theme.accent : T.panel2).color))
            if narrow { VStack(spacing: T.px(10)) { params } } else { params }
            IconButton(symbol: "xmark", label: L("Remove filter {index}", ["index": String(index + 1)]), disabled: !canRemove) {
                let id = filter.id
                editAndSave { p in p.filters.removeAll { $0.id == id }; p.parametricPreset = "" }
            }
        }
        .padding(.vertical, T.px(10))
        .padding(.leading, T.px(14))
        .padding(.trailing, T.px(8))
        .background(RoundedRectangle(cornerRadius: T.rMD, style: .continuous).fill(T.row.color))
        .overlay(RoundedRectangle(cornerRadius: T.rMD, style: .continuous).strokeBorder(active ? theme.accent.color : .clear, lineWidth: 1).allowsHitTesting(false))
    }

    private func param<Control: View>(_ label: String, _ value: String, @ViewBuilder control: () -> Control) -> some View {
        VStack(spacing: T.px(6)) {
            HStack {
                Text(label).foregroundStyle(T.t2.color)
                Spacer(minLength: T.px(8))
                Text(value).foregroundStyle(T.t1.color).monospacedDigit()
            }
            .font(.navia(T.textSM))
            .lineLimit(1)
            control().frame(maxWidth: .infinity)
        }
        .frame(maxWidth: .infinity)
    }

    // MARK: Headphone correction

    private var headphoneHint: String {
        if let importError { return importError }
        if sampleMode && state.headphones.isEmpty { return "Sample list with invented curves, or load an AutoEQ ParametricEQ.txt file." }
        return state.headphones.isEmpty ? L("Load an AutoEQ ParametricEQ.txt file. AutoEQ publishes one for most headphones.") : L("Choose a loaded AutoEQ profile, or load another ParametricEQ.txt file.")
    }

    private var headphoneOptions: [SelectOption] {
        var list = [SelectOption(value: "", label: L("None"))]
        if sampleMode { list += eqSampleHeadphones().map { SelectOption(value: $0.name, label: $0.name, group: $0.group) } }
        list += state.headphones.map { SelectOption(value: $0.name, label: $0.name, group: L($0.group)) }
        return list
    }

    private var headphoneControls: some View {
        HStack(spacing: T.px(8)) {
            GlassSelect(shell: shell, label: L("Headphone model"), placeholder: L("None"), value: profile.headphone, options: headphoneOptions, width: T.px(ctx.cssWidth <= 620 ? 220 : 280)) { value in
                editAndSave { $0.headphone = value }
            }
            ActionButton(title: L("Load File"), symbol: "square.and.arrow.up") { importFile() }
            if state.headphones.contains(where: { $0.name == profile.headphone }) {
                roundButton(symbol: "trash", label: L("Remove {name}", ["name": profile.headphone])) {
                    let name = profile.headphone
                    store.updateAndSave { s in
                        s.headphones.removeAll { $0.name == name }
                        for key in s.profiles.keys where s.profiles[key]?.headphone == name { s.profiles[key]?.headphone = "" }
                    }
                    shell.applyEqualizer()
                }
            }
        }
    }

    private func importFile() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = [UTType.plainText, UTType(filenameExtension: "txt") ?? .plainText]
        guard panel.runModal() == .OK, let url = panel.url else { return }
        let name = eqHeadphoneName(fromPath: url.path)
        guard let text = try? String(contentsOf: url, encoding: .utf8) else { importError = L("That file couldn't be read."); return }
        switch parseAutoEq(name: name.isEmpty ? L("AutoEQ profile") : name, text: text) {
        case .success(let headphone):
            importError = nil
            store.updateAndSave { s in
                s.headphones.removeAll { $0.name == headphone.name }
                s.headphones.append(headphone)
                s.ensureProfile()
                var p = s.current
                p.headphone = headphone.name
                s.current = p
            }
            shell.applyEqualizer()
        case .failure(let error):
            importError = error.message
        }
    }
}

/// The response curve. Graphic mode drags ten fixed bands, and parametric mode drags each filter's frequency and gain.
struct EqGraphView: View {
    let profile: EqProfile
    let correction: EqCorrection?
    let foreground: RGBA
    let accent: RGBA
    let onAccent: RGBA
    let narrow: Bool
    @Binding var activeFilter: UInt64?
    let onBand: (Int, Double) -> Void
    let onFilter: (Int, Double?, Double?, Double?) -> Void
    let onEnd: () -> Void

    private let ticks: [Double] = [12, 6, 0, -6, -12]
    @State private var held: Int?
    @State private var hover: Int?

    private var parametric: Bool { profile.mode == .parametric }

    var body: some View {
        let height = T.px(narrow ? 220 : 280)
        let scaleW = T.px(narrow ? 36 : 46)
        let curve = eqProfileCurve(profile)
        let corrected = correction.map(eqCorrectionCurve)
        return VStack(spacing: 0) {
            HStack(spacing: 0) {
                ZStack(alignment: .topTrailing) {
                    ForEach(ticks, id: \.self) { t in
                        Text(t == 0 ? "0 dB" : formatDb(t).replacingOccurrences(of: ".0", with: ""))
                            .font(.navia(T.textXS))
                            .foregroundStyle(T.t3.color)
                            .monospacedDigit()
                            .padding(.trailing, T.px(narrow ? 8 : 12))
                            .position(x: scaleW / 2, y: gainToY(t) * height)
                    }
                }
                .frame(width: scaleW, height: height)
                .padding(.vertical, T.px(8))
                plot(curve: curve, corrected: corrected, height: height)
                    .frame(height: height)
                    .padding(.vertical, T.px(8))
            }
            axis(scaleW: scaleW)
        }
    }

    private func plot(curve: [EqCurvePoint], corrected: [EqCurvePoint]?, height: CGFloat) -> some View {
        GeometryReader { geo in
            ZStack(alignment: .topLeading) {
                Canvas { context, size in draw(&context, size: size, curve: curve, corrected: corrected) }
                if parametric {
                    ForEach(Array(profile.filters.enumerated()), id: \.element.id) { index, f in
                        filterHandle(index: index, filter: f, in: geo.size)
                    }
                } else {
                    ForEach(eqBands.indices, id: \.self) { i in
                        bandHandle(i, in: geo.size)
                    }
                }
            }
            .contentShape(Rectangle())
            .gesture(DragGesture(minimumDistance: 0, coordinateSpace: .local)
                .onChanged { drag(start: $0.startLocation, location: $0.location, size: geo.size) }
                .onEnded { _ in
                    if held != nil { onEnd() }
                    held = nil
                })
            .onContinuousHover { phase in
                switch phase {
                case .active(let point): hover = nearest(point, size: geo.size, radius: T.px(28))
                case .ended: hover = nil
                }
            }
        }
    }

    private func draw(_ context: inout GraphicsContext, size: CGSize, curve: [EqCurvePoint], corrected: [EqCurvePoint]?) {
        func y(_ g: Double) -> CGFloat { gainToY(clampGain(g)) * size.height }
        func path(_ points: [EqCurvePoint]) -> Path {
            var p = Path()
            for (i, point) in points.enumerated() {
                let pt = CGPoint(x: point.x * size.width, y: y(point.gain))
                if i == 0 { p.move(to: pt) } else { p.addLine(to: pt) }
            }
            return p
        }
        for db in ticks {
            let color = db == 0 ? T.eqGridZero : T.eqGrid
            context.fill(Path(CGRect(x: 0, y: y(db), width: size.width, height: 1)), with: .color(color.color))
        }
        for f in eqBands {
            let x = freqToX(f) * size.width
            context.fill(Path(CGRect(x: x, y: 0, width: 1, height: size.height)), with: .color(T.eqGridBand.color))
        }
        var area = path(curve)
        area.addLine(to: CGPoint(x: size.width, y: size.height / 2))
        area.addLine(to: CGPoint(x: 0, y: size.height / 2))
        area.closeSubpath()
        context.fill(area, with: .color(foreground.opacity(T.eqAreaOpacity).color))
        if let corrected {
            context.stroke(path(corrected), with: .color(T.t2.color), style: StrokeStyle(lineWidth: 1.5, dash: [5, 5]))
        }
        context.stroke(path(curve), with: .color(foreground.color), style: StrokeStyle(lineWidth: 2.5, lineCap: .round, lineJoin: .round))
    }

    private func point(x: Double, gain: Double, in size: CGSize) -> CGPoint {
        CGPoint(x: x * size.width, y: gainToY(clampGain(gain)) * size.height)
    }

    private func bandHandle(_ i: Int, in size: CGSize) -> some View {
        Circle()
            .fill(T.t1.color)
            .frame(width: T.px(14), height: T.px(14))
            .shadow(T.shadowEqHandle)
            .overlay(Circle().strokeBorder(T.eqHandleRing.color, lineWidth: 1))
            .scaleEffect(held == i || hover == i ? 1.25 : 1)
            .position(point(x: freqToX(eqBands[i]), gain: profile.gains[i], in: size))
            .allowsHitTesting(false)
    }

    private func filterHandle(index: Int, filter f: EqFilter, in size: CGSize) -> some View {
        let active = f.id == activeFilter || held == index
        return Text("\(index + 1)")
            .font(.navia(T.textXS, .semibold))
            .foregroundStyle((active ? onAccent : T.bg).color)
            .frame(width: T.px(24), height: T.px(24))
            .background(Circle().fill((active ? accent : T.t1).color))
            .shadow(T.shadowEqHandle)
            .position(point(x: freqToX(f.freq), gain: f.gain, in: size))
            .allowsHitTesting(false)
    }

    private func axis(scaleW: CGFloat) -> some View {
        GeometryReader { geo in
            ForEach(eqBands.indices, id: \.self) { i in
                VStack(spacing: 0) {
                    Text(narrow ? eqBandShort[i] : eqBandLabels[i]).font(.navia(T.textXS)).foregroundStyle(T.t2.color).lineLimit(1)
                    if !parametric { Text(formatDb(profile.gains[i])).font(.navia(T.textXS)).foregroundStyle(T.t3.color).monospacedDigit().lineLimit(1) }
                }
                .fixedSize()
                .position(x: freqToX(eqBands[i]) * geo.size.width, y: T.px(20))
            }
        }
        .frame(height: T.px(40))
        .padding(.leading, scaleW)
    }

    /// in parametric mode a drag only starts on a filter's dot.
    private func nearest(_ location: CGPoint, size: CGSize, radius: CGFloat) -> Int? {
        let x = min(1, max(0, location.x / max(1, size.width)))
        if parametric {
            var best: Int?
            var closest = radius
            for (i, f) in profile.filters.enumerated() {
                let d = hypot((freqToX(f.freq) - x) * size.width, gainToY(f.gain) * size.height - location.y)
                if d <= closest { closest = d; best = i }
            }
            return best
        }
        return eqBands.indices.min { abs(freqToX(eqBands[$0]) - x) < abs(freqToX(eqBands[$1]) - x) }
    }

    private func drag(start: CGPoint, location: CGPoint, size: CGSize) {
        if held == nil {
            held = nearest(start, size: size, radius: T.px(28))
            if parametric, let held { activeFilter = profile.filters[held].id }
        }
        guard let held else { return }
        let x = min(1, max(0, location.x / max(1, size.width)))
        let y = min(1, max(0, location.y / max(1, size.height)))
        if parametric {
            onFilter(held, snapFreq(xToFreq(x)), snapGain(yToGain(y)), nil)
        } else {
            onBand(held, snapGain(yToGain(y)))
        }
    }
}
