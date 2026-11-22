import AppKit
import NaviaCoreFFI
import SwiftUI

/// groups show as headings, in the order of the options.
struct SelectOption: Identifiable, Equatable {
    var value: String
    var label: String
    var group: String? = nil
    var id: String { value }
}

/// drawn by `RootView` above every page.
@Observable final class PopupCenter {
    struct Request {
        let label: String
        let value: String
        let options: [SelectOption]
        /// window coordinates, from the top left.
        let anchor: CGRect
        let choose: (String) -> Void
    }

    var request: Request?
    /// A smart playlist's rules, opened from the playlist header.
    var rules: RulesRequest?

    struct RulesRequest {
        let playlistID: String
        let summary: RuleSummary
        let anchor: CGRect
        let onEdit: (() -> Void)?
        /// Someone else's rules, opened in the editor to be saved as a copy.
        var copy = false
    }

    /// A filter's popover, with which filter and under which chip.
    struct FilterRequest {
        let id: SongFilterId
        let anchor: CGRect
        let filters: FiltersModel
    }
    var filter: FilterRequest?

    func open(_ request: Request) { rules = nil; filter = nil; self.request = request }
    func open(_ rules: RulesRequest) { request = nil; filter = nil; self.rules = rules }
    func open(_ filter: FilterRequest) { request = nil; rules = nil; self.filter = filter }
    func close() { request = nil; rules = nil; quality = nil; filter = nil }

    /// The quality badge's details, shown over the badge at `anchor` in window coordinates from the top left.
    /// Pinned by a click, or open while the pointer rests on the badge or the details.
    struct QualityRequest {
        let anchor: CGRect
        let pinned: Bool
    }
    var quality: QualityRequest?
    @ObservationIgnored private var qualityClose: Task<Void, Never>?

    func openQuality(anchor: CGRect, pinned: Bool) {
        qualityClose?.cancel()
        quality = QualityRequest(anchor: anchor, pinned: pinned)
    }

    func closeQuality(unlessPinned: Bool) {
        qualityClose?.cancel()
        if unlessPinned && quality?.pinned == true { return }
        quality = nil
    }

    /// Cancels a pending close while the pointer is on the details.
    func keepQuality() { qualityClose?.cancel() }

    func closeQualitySoon() {
        qualityClose?.cancel()
        qualityClose = Task { @MainActor [weak self] in
            try? await Task.sleep(for: QualityBadge.hoverClose)
            guard !Task.isCancelled else { return }
            self?.closeQuality(unlessPinned: true)
        }
    }
}

/// A glass pill that opens a floating glass list. Long lists get a filter field.
struct GlassSelect: View {
    let shell: ShellModel
    let label: String
    var placeholder = L("Choose")
    let value: String
    let options: [SelectOption]
    var width: CGFloat? = nil
    /// Disabled controls show their reason in a tooltip.
    var disabledReason: String? = nil
    let onChange: (String) -> Void
    @State private var frame: CGRect = .zero
    @State private var hovering = false

    private var current: SelectOption? { options.first { $0.value == value } }

    var body: some View {
        let isOpen = shell.popups.request?.label == label
        Button {
            guard disabledReason == nil else { return }
            if isOpen { shell.popups.close(); return }
            shell.popups.open(.init(label: label, value: value, options: options, anchor: frame, choose: onChange))
        } label: {
            HStack(spacing: T.px(6)) {
                Text(current?.label ?? (value.isEmpty ? placeholder : value))
                    .font(.navia(T.textMD))
                    .foregroundStyle((current == nil && value.isEmpty ? T.t3 : T.t1).color)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Image(systemName: "chevron.up.chevron.down")
                    .font(.system(size: T.px(11), weight: .medium))
                    .foregroundStyle(T.t2.color)
            }
            .padding(.leading, T.px(16))
            .padding(.trailing, T.px(12))
            .frame(height: T.controlH)
            .frame(width: width)
            .frame(maxWidth: width == nil ? .infinity : nil)
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .glassPill()
        .overlay(Capsule().fill(hovering && disabledReason == nil ? T.hover.color : .clear).allowsHitTesting(false))
        .focusRing(isOpen, shell.theme.accent)
        .opacity(disabledReason == nil ? 1 : T.disabledOpacity)
        .disabled(disabledReason != nil)
        .help(disabledReason ?? (current?.label ?? label))
        .onHover { hovering = $0 }
        .onGeometryChange(for: CGRect.self, of: { $0.frame(in: .global) }) { frame = $0 }
        .accessibilityLabel(label)
        .accessibilityValue(current?.label ?? "")
    }
}

/// The list itself. Adds a filter field at 9 or more options, handles the arrow keys, Return and Esc, and closes
/// on a click outside.
struct SelectPopupLayer: View {
    let request: PopupCenter.Request
    let center: PopupCenter
    let accent: RGBA
    let windowSize: CGSize

    private static var rowH: CGFloat { T.px(34) }
    private static let filterFrom = 9
    @State private var query = ""
    @State private var active = 0
    @FocusState private var filterFocused: Bool
    @FocusState private var listFocused: Bool

    private var filtering: Bool { request.options.count >= Self.filterFrom }

    private var shown: [SelectOption] {
        let q = query.trimmingCharacters(in: .whitespaces).lowercased()
        guard !q.isEmpty else { return request.options }
        return request.options.filter { $0.label.lowercased().contains(q) || ($0.group?.lowercased().contains(q) ?? false) }
    }

    /// The list's height, so it can flip above the pill when it wouldn't fit.
    private var contentHeight: CGFloat {
        var headings = 0, last: String? = nil
        for o in shown where o.group != nil && o.group != last { headings += 1; last = o.group }
        return CGFloat(shown.count) * Self.rowH + CGFloat(headings) * T.px(30) + T.px(12) + (filtering ? T.px(38) : 0)
    }

    private var width: CGFloat { max(request.anchor.width, T.px(240)) }

    var body: some View {
        let edge = T.px(10), gap = T.px(6)
        let below = windowSize.height - request.anchor.maxY - gap - edge
        let above = request.anchor.minY - gap - edge
        let flip = contentHeight > below && above > below
        let maxHeight = max(T.px(160), flip ? above : below)
        let height = min(contentHeight, maxHeight)
        let x = max(edge, min(request.anchor.minX, windowSize.width - width - edge))
        let y = flip ? max(edge, request.anchor.minY - gap - height) : request.anchor.maxY + gap

        return ZStack(alignment: .topLeading) {
            Color.clear
                .contentShape(Rectangle())
                .onTapGesture { center.close() }
            list
                .frame(width: width, height: height)
                .floatPanel()
                .offset(x: x, y: y)
        }
        .frame(width: windowSize.width, height: windowSize.height, alignment: .topLeading)
        .onAppear {
            active = max(0, request.options.firstIndex { $0.value == request.value } ?? 0)
            if filtering { filterFocused = true } else { listFocused = true }
        }
    }

    private var list: some View {
        VStack(spacing: T.px(4)) {
            if filtering {
                HStack(spacing: T.px(8)) {
                    Image(systemName: "magnifyingglass").font(.system(size: T.px(13))).foregroundStyle(T.t2.color)
                    TextField("", text: $query, prompt: Text(L("Filter")).foregroundStyle(T.t3.color))
                        .textFieldStyle(.plain)
                        .font(.navia(T.textSM))
                        .focused($filterFocused)
                        .onChange(of: query) { _, _ in active = 0 }
                        .onKeyPress(.upArrow) { step(-1) }
                        .onKeyPress(.downArrow) { step(1) }
                        .onKeyPress(.return) { commit() }
                        .onKeyPress(.escape) { center.close(); return .handled }
                    SearchClearButton(text: $query, size: T.searchClearSM)
                }
                .padding(.horizontal, T.px(11))
                .frame(height: T.px(34))
                .background(RoundedRectangle(cornerRadius: T.px(10), style: .continuous).fill(T.row.color))
            }
            rows
                .focusable(!filtering)
                .focusEffectDisabled()
                .focused($listFocused)
                .onKeyPress(.upArrow) { step(-1) }
                .onKeyPress(.downArrow) { step(1) }
                .onKeyPress(.return) { commit() }
                .onKeyPress(.escape) { center.close(); return .handled }
        }
        .padding(T.px(6))
    }

    private var rows: some View {
        ScrollViewReader { proxy in
            ScrollView(.vertical) {
                LazyVStack(alignment: .leading, spacing: 0) {
                    let items = shown
                    if items.isEmpty {
                        Text(L("No matches")).font(.navia(T.textSM)).foregroundStyle(T.t2.color)
                            .padding(.vertical, T.px(12)).padding(.horizontal, T.px(11))
                    }
                    ForEach(Array(items.enumerated()), id: \.element.id) { index, option in
                        if let heading = option.group, heading != (index > 0 ? items[index - 1].group : nil) {
                            Text(heading).font(.navia(T.textXS, .semibold)).foregroundStyle(T.t2.color)
                                .padding(.top, T.px(10)).padding(.bottom, T.px(4)).padding(.horizontal, T.px(11))
                        }
                        row(option, index: index).id(option.id)
                    }
                }
            }
            .scrollIndicators(.never)
            .onChange(of: active) { _, _ in
                let items = shown
                if items.indices.contains(active) { proxy.scrollTo(items[active].id) }
            }
            .onAppear {
                if request.options.contains(where: { $0.value == request.value }) { proxy.scrollTo(request.value, anchor: .center) }
            }
        }
    }

    private func row(_ option: SelectOption, index: Int) -> some View {
        Button { choose(option) } label: {
            HStack(spacing: T.px(10)) {
                Text(option.label).font(.navia(T.textMD)).foregroundStyle(T.t1.color).lineLimit(1).truncationMode(.tail).frame(maxWidth: .infinity, alignment: .leading)
                if option.value == request.value {
                    Image(systemName: "checkmark").font(.system(size: T.px(13), weight: .bold)).foregroundStyle(accent.color)
                }
            }
            .padding(.horizontal, T.px(11))
            .frame(height: Self.rowH)
            .background(RoundedRectangle(cornerRadius: T.px(10), style: .continuous).fill(index == active ? T.hover.color : .clear))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { if $0 { active = index } }
    }

    private func step(_ delta: Int) -> KeyPress.Result {
        let count = shown.count
        guard count > 0 else { return .handled }
        active = min(count - 1, max(0, active + delta))
        return .handled
    }

    private func commit() -> KeyPress.Result {
        let items = shown
        if items.indices.contains(active) { choose(items[active]) }
        return .handled
    }

    private func choose(_ option: SelectOption) {
        let pick = request.choose
        let previous = request.value
        center.close()
        if option.value != previous { pick(option.value) }
    }
}
