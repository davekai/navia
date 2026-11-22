import NaviaCoreFFI
import SwiftUI

/// A page's filter row (Songs and Artists): one line of chips under the top bar that never wraps. A chip opens its
/// editor in a popover. Chips that don't fit fold into More Filters, and so do Clear All and Save when there's no
/// room for them.
struct FilterRow: View {
    let shell: ShellModel
    let filters: FiltersModel
    /// Whether the page offers Save as Smart Playlist. Navidrome's smart playlists are rules over songs.
    var offersSave = false
    /// Save as Smart Playlist. Nil while a typed range is unfinished.
    var save: (() -> Void)? = nil
    /// Every chip's natural width (measured unseen), plus the More chip's, the end's and the line's.
    @State private var widths: [SongFilterId: CGFloat] = [:]
    @State private var moreW: CGFloat = 0
    @State private var endW: CGFloat = 0
    @State private var room: CGFloat = 0

    var body: some View {
        let candidates = filters.shown
        let opening = shell.popups.filter.flatMap { $0.filters === filters ? $0.id : nil } ?? filters.pendingOpen
        let fold = ChipFold.fold(widths: candidates.map { widths[$0] ?? 0 }, keep: candidates.map { filters.value($0) != nil },
                                 pin: opening.flatMap { candidates.firstIndex(of: $0) } ?? -1, moreW: moreW,
                                 endW: filters.isFiltering ? endW : 0, gap: T.chipGap, room: room, rest: !filters.rest.isEmpty)
        let folded = zip(candidates, fold.shown).filter { !$0.1 }.map(\.0)
        HStack(spacing: T.chipGap) {
            ForEach(Array(zip(candidates, fold.shown)).filter(\.1).map(\.0), id: \.self) { id in
                FilterChipView(shell: shell, filters: filters, id: id)
            }
            if fold.more { more(folded, end: fold.endInMore && filters.isFiltering) }
            if filters.isFiltering && !fold.endInMore {
                Spacer(minLength: 0)
                end
            }
        }
        // `minWidth: 0`, or a line that's too long would widen the page instead of folding.
        .frame(minWidth: 0, maxWidth: .infinity, minHeight: T.chipH, maxHeight: T.chipH, alignment: .leading)
        .onGeometryChange(for: CGFloat.self, of: { $0.size.width }) { room = $0 }
        .background(alignment: .topLeading) { measure(candidates) }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(L("Filters"))
    }

    /// Folded chips in line order, then the filters outside the main set, then Clear All and Save if they didn't fit.
    /// A picked filter joins the line and opens.
    private func more(_ folded: [SongFilterId], end: Bool) -> some View {
        FilterChip(menu: true, action: {}) {
            Image(systemName: "plus").font(.system(size: T.chipGlyph, weight: .bold))
            Text(L("More Filters"))
        }
        .overlay {
            MenuHitArea {
                let chips = folded.map { id -> MenuEntry in
                    let info = filters.info(id), value = filters.value(id)
                    let title = value.map { info.kind == .toggle ? L(info.label) : L(info.label) + ": " + filters.summary(id, $0) } ?? L(info.label)
                    return MenuEntry(title: title, checked: value != nil) {
                        if info.kind == .toggle { filters.set(id, value == nil ? .on : nil) } else { filters.pendingOpen = id }
                    }
                }
                let rest = filters.rest.map { id in
                    MenuEntry(title: L(filters.info(id).label)) {
                        if !filters.added.contains(id) { filters.added.append(id) }
                        if filters.info(id).kind == .toggle { filters.set(id, .on) } else { filters.pendingOpen = id }
                    }
                }
                var ends: [MenuEntry] = []
                if end {
                    ends = [.divider, MenuEntry(title: L("Clear All")) { filters.clear() }]
                    if offersSave { ends.append(MenuEntry(title: L("Save as Smart Playlist"), enabled: save != nil) { save?() }) }
                }
                return chips + rest + ends
            }
        }
    }

    private var end: some View {
        HStack(spacing: T.chipGap) {
            TextPillButton(symbol: nil, title: L("Clear All")) { filters.clear() }
            if offersSave {
                ActionButton(title: L("Save as Smart Playlist"), symbol: "bolt.fill", kind: .primary(fill: shell.theme.accent, ink: shell.theme.onAccent), small: true, enabled: save != nil) { save?() }
            }
        }
        .fixedSize()
    }

    /// unseen, so the line knows what folds.
    private func measure(_ candidates: [SongFilterId]) -> some View {
        HStack(spacing: 0) {
            ForEach(candidates, id: \.self) { id in
                FilterChipView(shell: shell, filters: filters, id: id, measuring: true)
                    .onGeometryChange(for: CGFloat.self, of: { $0.size.width }) { widths[id] = $0 }
            }
            more([], end: false).onGeometryChange(for: CGFloat.self, of: { $0.size.width }) { moreW = $0 }
            end.onGeometryChange(for: CGFloat.self, of: { $0.size.width }) { endW = $0 }
        }
        .fixedSize()
        .hidden()
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

/// One filter's chip, which knows its place so its popover can open under it. `measuring` makes an unseen copy for
/// measuring the fold.
private struct FilterChipView: View {
    let shell: ShellModel
    let filters: FiltersModel
    let id: SongFilterId
    var measuring = false
    @State private var probe = WindowFrameProbe()
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        let info = filters.info(id)
        let value = filters.value(id)
        Group {
            if info.kind == .toggle {
                FilterChip(on: value != nil, action: { filters.set(id, value == nil ? .on : nil) }) { Text(L(info.label)) }
            } else if let value {
                FilterChip(on: true, menu: true, remove: { filters.set(id, nil) }, removeLabel: L("Clear {filter}", ["filter": L(info.label)]), action: toggle) {
                    Text(L(info.label) + ":").opacity(0.6)
                    ChipValue(text: filters.summary(id, value))
                }
            } else {
                FilterChip(menu: true, action: toggle) { Text(L(info.label)) }
            }
        }
        .background { if !isSnapshot && !measuring { WindowFrameReader(probe: probe) } }
        .onChange(of: filters.pendingOpen) { _, next in if !measuring { openIfPending(next) } }
        .onAppear { if !measuring { openIfPending(filters.pendingOpen) } }
    }

    private func toggle() {
        if shell.popups.filter?.id == id { shell.popups.close(); return }
        shell.popups.open(.init(id: id, anchor: probe.frame(), filters: filters))
    }

    /// A filter picked in More Filters opens once its chip is in place.
    private func openIfPending(_ pending: SongFilterId?) {
        guard pending == id else { return }
        DispatchQueue.main.async {
            filters.pendingOpen = nil
            shell.popups.open(.init(id: id, anchor: probe.frame(), filters: filters))
        }
    }
}

struct FilterPopoverLayer: View {
    let request: PopupCenter.FilterRequest
    let center: PopupCenter
    let accent: RGBA
    let windowSize: CGSize
    @State private var contentHeight: CGFloat = 0

    var body: some View {
        let filters = request.filters
        AnchoredPopover(anchor: request.anchor, width: T.filterPopW, wanted: contentHeight, windowSize: windowSize, minHeight: min(T.px(120), contentHeight), close: { center.close() }) {
            VStack(spacing: 0) {
                Group {
                    if filters.info(request.id).kind == .range {
                        FilterRangeEditor(filters: filters, id: request.id, accent: accent)
                    } else {
                        FilterListEditor(filters: filters, id: request.id, accent: accent)
                    }
                }
                .padding(T.dialogGap)
                if filters.value(request.id) != nil {
                    Rectangle().fill(T.line.color).frame(height: 1)
                    HStack {
                        Spacer(minLength: 0)
                        TextPillButton(symbol: nil, title: L("Clear")) { filters.set(request.id, nil) }
                    }
                    .padding(T.chipGap)
                }
            }
            .fixedSize(horizontal: false, vertical: true)
            .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { contentHeight = $0 }
            .accessibilityElement(children: .contain)
            .accessibilityLabel(L(filters.info(request.id).label))
        }
        .id(request.id)
    }
}

/// Short lists as chips. Long ones as a find field over a checked list with counts, picked ones first.
private struct FilterListEditor: View {
    let filters: FiltersModel
    let id: SongFilterId
    let accent: RGBA
    @State private var query = ""
    private static var rowH: CGFloat { T.px(36) }

    var body: some View {
        let info = filters.info(id)
        let options = filters.options(id)
        let picked: [String] = { if case .list(let values) = filters.value(id) { return values }; return [] }()
        if info.find == nil && options.count <= 6 {
            FlowChips(spacing: T.chipGap, lineSpacing: T.chipGap) {
                ForEach(options, id: \.value) { option in
                    FilterChip(on: picked.contains(option.value), action: { filters.toggleChoice(id, option.value) }) { Text(filters.label(option)) }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        } else {
            let needle = query.trimmingCharacters(in: .whitespaces).lowercased()
            let shown = options.filter { picked.contains($0.value) } + options.filter { !picked.contains($0.value) && (needle.isEmpty || filters.label($0).lowercased().contains(needle)) }
            VStack(spacing: T.px(4)) {
                HStack(spacing: T.px(8)) {
                    Image(systemName: "magnifyingglass").font(.system(size: T.px(13))).foregroundStyle(T.t2.color)
                    TextField("", text: $query, prompt: Text(L(info.find ?? N_("Find"))).foregroundStyle(T.t3.color))
                        .textFieldStyle(.plain)
                        .font(.navia(T.textSM))
                        .accessibilityLabel(L(info.find ?? N_("Find")))
                    SearchClearButton(text: $query, size: T.searchClearSM)
                }
                .padding(.horizontal, T.px(11))
                .frame(height: T.px(34))
                .background(RoundedRectangle(cornerRadius: T.px(10), style: .continuous).fill(T.row.color))
                ScrollView(.vertical) {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        if shown.isEmpty {
                            Text(L("No matches")).font(.navia(T.textSM)).foregroundStyle(T.t2.color).padding(T.listRowPadX)
                        }
                        ForEach(shown, id: \.value) { option in
                            row(option, on: picked.contains(option.value))
                        }
                    }
                }
                .scrollIndicators(.automatic)
                .frame(height: min(CGFloat(max(shown.count, 1)) * Self.rowH, T.filterListMaxH))
            }
        }
    }

    private func row(_ option: SongFilterOptionRow, on: Bool) -> some View {
        Button { filters.toggleChoice(id, option.value) } label: {
            HStack(spacing: T.px(11)) {
                Text(filters.label(option)).font(.navia(T.textMD)).foregroundStyle(T.t1.color).lineLimit(1).truncationMode(.tail).frame(maxWidth: .infinity, alignment: .leading)
                Text(fmtInt(Int(option.count))).font(.navia(T.textXS).monospacedDigit()).foregroundStyle(T.t3.color)
                if on { Circle().fill(accent.color).frame(width: T.px(7), height: T.px(7)) }
            }
            .padding(.horizontal, T.px(11))
            .frame(height: Self.rowH)
            .contentShape(Rectangle())
        }
        .buttonStyle(HoverFillStyle(key: option.value, shrinks: false))
        .accessibilityAddTraits(on ? .isSelected : [])
    }
}

/// Quick picks (decades) as chips, then From and To, with what's wrong while they can't be used.
private struct FilterRangeEditor: View {
    let filters: FiltersModel
    let id: SongFilterId
    let accent: RGBA

    var body: some View {
        let (from, to): (String, String) = { if case .range(let f, let t) = filters.value(id) { return (f, t) }; return ("", "") }()
        let problem = songFilterRangeProblem(id: id, from: from, to: to)
        let quick = filters.quick(id)
        VStack(alignment: .leading, spacing: T.sheetLabelGap) {
            if !quick.isEmpty {
                FlowChips(spacing: T.chipGap, lineSpacing: T.chipGap) {
                    ForEach(quick, id: \.label) { q in
                        let on = from == q.from && to == q.to
                        FilterChip(on: on, action: { filters.set(id, on ? nil : .range(from: q.from, to: q.to)) }) { Text(q.label) }
                    }
                }
            }
            HStack(spacing: T.sheetLabelGap) {
                NumberField(placeholder: L("From"), text: bound(from: true, from, to), bad: problem != nil && !from.isEmpty, accent: accent)
                Text(L("to")).font(.navia(T.textSM)).foregroundStyle(T.t2.color)
                NumberField(placeholder: L("To"), text: bound(from: false, from, to), bad: problem != nil && !to.isEmpty, accent: accent)
            }
            if let problem {
                Text(L(problem)).font(.navia(T.textXS)).foregroundStyle(T.t2.color)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func bound(from isFrom: Bool, _ from: String, _ to: String) -> Binding<String> {
        Binding(get: { isFrom ? from : to }, set: { typed in
            let clean = songFilterCleanInput(id: id, text: typed)
            filters.set(id, .range(from: isFrom ? clean : from, to: isFrom ? to : clean))
        })
    }
}
