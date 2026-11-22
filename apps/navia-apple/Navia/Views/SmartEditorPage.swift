import NaviaCoreFFI
import SwiftUI

private let previewMax = 200

/// The editor's model: the core's `NaviaSmartEditor` and a preview over the local library. Every edit goes to the
/// core, then the page reads the rows back.
@Observable final class SmartEditorModel {
    var name = ""
    private(set) var rows: [SmartRowView] = []
    private(set) var sorts: [SmartSortView] = []
    private(set) var matchAny = false
    private(set) var limit = ""
    private(set) var limitPercent = false
    private(set) var albumHintAt: UInt32?
    private(set) var sentence = ""
    private(set) var criteria = ""
    private(set) var hasRules = false
    private(set) var complete = true
    private(set) var preview = SmartPreviewResult(rowids: [], matched: 0, sampled: 0, unchecked: [], checkable: false, sortUnchecked: [])
    private(set) var songs: [SongRow] = []
    private(set) var loading = true
    /// Filters that Save as Smart Playlist couldn't carry over because the server has no field for them.
    var dropped: String?
    @ObservationIgnored private(set) var editor = NaviaSmartEditor(criteria: nil, words: SmartText())
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var library: (any LibraryViewModel)?

    func attach(_ library: any LibraryViewModel) { self.library = library }

    func load(_ criteria: String?, playlists: [(id: String, name: String)]) {
        editor = NaviaSmartEditor(criteria: criteria, words: SmartText(playlists: playlists))
        sync()
    }

    func edit(_ change: (NaviaSmartEditor) -> Void) {
        change(editor)
        sync()
        refresh()
    }

    private func sync() {
        rows = editor.rows()
        sorts = editor.sorts()
        matchAny = editor.matchAny()
        limit = editor.limit()
        limitPercent = editor.limitIsPercent()
        albumHintAt = editor.albumHintAt()
        sentence = editor.sentence()
        criteria = editor.criteria()
        hasRules = editor.hasRules()
        complete = editor.rowsComplete()
    }

    /// runs after typing pauses; newer runs supersede older ones.
    func refresh(delay: Bool = true) {
        generation &+= 1
        let current = generation
        loading = true
        let editor = editor
        Task { [weak self] in
            if delay { try? await Task.sleep(for: .milliseconds(150)) }
            guard let self, self.generation == current, let library = self.library else { return }
            let result = await library.smartPreview(editor: editor, maxRows: previewMax)
            guard self.generation == current else { return }
            self.preview = result.preview
            self.songs = result.songs
            self.loading = false
        }
    }
}

struct SmartEditorPage: View {
    let shell: ShellModel
    let id: String?
    @State private var model = SmartEditorModel()
    @State private var didLoad = false
    /// The name and rules as opened, to tell whether anything changed.
    @State private var opened: String?
    @State private var saving = false
    /// Whether an existing playlist's rules were known on open. Without them, saving would replace the real rules.
    @State private var rulesLoaded = false
    /// Why the last save failed, shown under the name until the next edit.
    @State private var problem: String?

    var body: some View {
        let existing = id.flatMap { shell.library.playlist(id: $0) }
        if id != nil, existing == nil || !existing!.isSmart {
            PageFrame(shell: shell, nav: .none, title: L("Edit Smart Playlist")) {
                PageScroll { EmptyStateView(symbol: "questionmark", title: L("This smart playlist isn't in your library"), actionTitle: L("Back")) { leave() } }
            }
        } else {
            editor(existing)
        }
    }

    private func leave() {
        if shell.canGoBack { shell.goBack() } else { shell.go(id != nil ? .playlist(id!) : .playlists) }
    }

    private var trimmedName: String { model.name.trimmingCharacters(in: .whitespaces) }
    private var snapshot: String { trimmedName + "\n" + model.criteria }

    /// why Save or Save As can't save these rules now, or nil.
    private func rulesBlocked(_ existing: PlaylistRow?) -> String? {
        if saving { return L("Saving…") }
        if existing != nil, !rulesLoaded {
            return L("This playlist's rules haven't loaded from the server yet. Open it from Playlists, then come back here.")
        }
        if !model.hasRules { return L("Add a rule to save") }
        if !model.complete { return L("Finish or remove the unfinished rules to save") }
        return shell.library.smartRulesProblem(name: trimmedName.isEmpty ? (existing?.name ?? "-") : trimmedName, criteria: model.criteria)
    }

    private func saveBlocked(_ existing: PlaylistRow?) -> String? {
        if trimmedName.isEmpty, !saving { return L("Add a name to save") }
        if let reason = rulesBlocked(existing) { return reason }
        if snapshot == opened { return L("Nothing has changed yet") }
        return nil
    }

    private func save(_ existing: PlaylistRow?) {
        guard saveBlocked(existing) == nil else { return }
        let (name, criteria, created) = (trimmedName, model.criteria, existing == nil)
        saving = true
        problem = nil
        Task {
            let result = await shell.library.saveSmartPlaylist(id: existing?.id, name: name, criteria: criteria)
            saving = false
            if case .failure(let failure) = result {
                problem = failure.message
                return
            }
            shell.showToast(created ? L("Created {name}", ["name": name]) : L("Saved {name}", ["name": name]))
            leave()
        }
    }

    /// Save As: saves a copy of the rules as a new smart playlist of the user's own, and opens it.
    private func saveAs(_ existing: PlaylistRow) {
        guard rulesBlocked(existing) == nil else { return }
        let current = trimmedName.isEmpty ? existing.name : trimmedName
        shell.openSmartSaveAs(name: existing.isOwned ? L("{name} Copy", ["name": current]) : current, criteria: model.criteria)
    }

    private func editor(_ existing: PlaylistRow?) -> some View {
        let others = existing.map { !$0.isOwned } ?? false
        let blocked = saveBlocked(existing)
        let asBlocked = rulesBlocked(existing)
        let title = existing == nil ? L("New Smart Playlist") : others ? L("Smart Playlist Rules") : L("Edit Smart Playlist")
        return PageFrame(shell: shell, nav: .none, title: title) {
            HStack(spacing: T.px(10)) {
                ActionButton(title: L("Cancel")) { leave() }
                if let existing {
                    ActionButton(title: L("Save As…"), kind: others ? .prominent(tint: shell.theme.accent) : .secondary, enabled: asBlocked == nil) { saveAs(existing) }
                        .help(asBlocked ?? "")
                }
                if !others {
                    ActionButton(title: saving ? L("Saving…") : L("Save"), kind: .prominent(tint: shell.theme.accent), enabled: blocked == nil) { save(existing) }
                        .help(blocked ?? "")
                }
            }
        } content: {
            PageScroll {
                SmartEditorForm(shell: shell, model: model, existing: existing, problem: problem)
            }
        }
        .onChange(of: snapshot) { _, _ in problem = nil }
        .task(id: existing?.id ?? "new") { load(existing) }
    }

    private func load(_ existing: PlaylistRow?) {
        guard !didLoad else { return }
        didLoad = true
        model.attach(shell.library)
        let names = shell.library.playlists.map { (id: $0.id, name: $0.name) }
        if let existing {
            model.name = existing.name
            let rules = shell.library.smartRulesJSON(playlistID: existing.id)
            rulesLoaded = rules != nil
            model.load(rules, playlists: names)
        } else if let prefill = shell.smartPrefill {
            // A new playlist from the Songs filters starts with their rules.
            shell.smartPrefill = nil
            model.load(prefill.rules, playlists: names)
            model.dropped = prefill.dropped
        } else {
            model.load(nil, playlists: names)
        }
        opened = snapshot
        #if DEBUG
        // For checks: `-naviaSmartRulesFile <path>` (Navidrome's JSON) fills the rules and `-naviaSmartName` the name.
        if let path = UserDefaults.standard.string(forKey: "naviaSmartRulesFile"), let rules = try? String(contentsOfFile: path, encoding: .utf8), existing == nil {
            model.load(rules, playlists: names)
        }
        if let name = UserDefaults.standard.string(forKey: "naviaSmartName") { model.name = name }
        #endif
        model.refresh(delay: false)
    }
}

private struct SmartEditorForm: View {
    let shell: ShellModel
    let model: SmartEditorModel
    let existing: PlaylistRow?
    let problem: String?
    @Environment(\.pageContext) private var ctx
    @FocusState private var nameFocused: Bool

    private var theme: Theme { shell.theme }
    private var others: PlaylistRow? { existing.flatMap { $0.isOwned ? nil : $0 } }

    var body: some View {
        VStack(alignment: .leading, spacing: T.px(28)) {
            VStack(alignment: .leading, spacing: T.px(12)) {
                nameField
                summary
                if let others {
                    Text(L("This playlist belongs to {owner}. Change the rules and use Save As to keep a copy of your own; the original stays as it is.", ["owner": others.ownerName]))
                        .font(.navia(T.textSM)).foregroundStyle(T.t2.color).fixedSize(horizontal: false, vertical: true)
                }
                if let problem { ErrorText(text: problem) }
            }
            rulesSection
            orderSection
            previewSection
            previewList
        }
        .padding(.leading, T.gutter)
        .padding(.trailing, T.gutter)
        .onChange(of: shell.library.collectionRevision) { _, _ in model.refresh(delay: false) }
    }

    // MARK: Name and summary

    @ViewBuilder private var nameField: some View {
        if let others {
            Text(others.name)
                .font(.navia(T.textTitle, .bold))
                .tracking(-0.024 * T.textTitle)
                .padding(.bottom, T.px(8))
                .frame(maxWidth: .infinity, alignment: .leading)
                .overlay(alignment: .bottom) { Rectangle().fill(T.line.color).frame(height: 1) }
        } else {
            TextField("", text: Binding(get: { model.name }, set: { model.name = $0 }), prompt: Text(L("Playlist Name")).foregroundStyle(T.t3.color))
                .textFieldStyle(.plain)
                .font(.navia(T.textTitle, .bold))
                .tracking(-0.024 * T.textTitle)
                .focused($nameFocused)
                .padding(.bottom, T.px(8))
                .overlay(alignment: .bottom) { Rectangle().fill((nameFocused ? theme.accent : T.line).color).frame(height: 1) }
        }
    }

    /// The rules in one line, as the playlist page says them.
    private var summary: some View {
        let empty = model.sentence.isEmpty
        return HStack(alignment: .firstTextBaseline, spacing: T.px(8)) {
            Image(systemName: "bolt.fill").font(.system(size: T.px(14))).foregroundStyle(theme.accent.color)
            Text(empty ? L("Add rules and they are summed up here.") : model.sentence)
                .font(.navia(T.textLG))
                .foregroundStyle((empty ? T.t3 : T.t1).color)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    // MARK: Rules

    private var rulesSection: some View {
        VStack(alignment: .leading, spacing: T.px(14)) {
            HStack(spacing: T.px(16)) {
                heading(L("Rules"))
                SegmentedChoice(look: .glass, value: model.matchAny, options: [.init(value: false, label: L("All of these")), .init(value: true, label: L("Any of these"))],
                                accent: theme.accent, label: L("Match")) { any in model.edit { $0.setAny(group: nil, any: any) } }
            }
            if model.rows.isEmpty {
                Text(L("No rules yet. Add one to start matching songs."))
                    .font(.navia(T.textMD))
                    .foregroundStyle(T.t2.color)
                    .padding(.vertical, T.px(14))
                    .padding(.horizontal, T.px(16))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(RoundedRectangle(cornerRadius: T.rMD, style: .continuous).fill(T.row.color))
            } else {
                VStack(spacing: T.smartRowGap) {
                    ForEach(model.rows, id: \.key) { row in
                        rowView(row).padding(.leading, T.smartGroupIndent * CGFloat(row.depth))
                    }
                }
            }
            HStack(spacing: T.smartRowGap) {
                ActionButton(title: L("Add Rule"), symbol: "plus") { model.edit { _ = $0.addRule(group: nil) } }
                ActionButton(title: L("Add Group"), symbol: "plus") { model.edit { _ = $0.addGroup(group: nil) } }
            }
            if let dropped = model.dropped {
                HStack(spacing: T.px(6)) {
                    Image(systemName: "exclamationmark.triangle.fill").font(.system(size: T.px(15)))
                    Text(L("Not carried over: {what}. The server has no field for it.", ["what": dropped]))
                }
                .font(.navia(T.textSM))
                .foregroundStyle(T.t2.color)
            }
        }
    }

    @ViewBuilder private func rowView(_ row: SmartRowView) -> some View {
        switch row.kind {
        case .group:
            VStack(alignment: .leading, spacing: T.smartRowGap) {
                SmartGroupHead(shell: shell, row: row, model: model)
                if row.empty {
                    // An empty group isn't a problem, since saving leaves it out. It offers what can go in it.
                    HStack(spacing: T.smartRowGap) {
                        ActionButton(title: L("Add Rule"), symbol: "plus", small: true) { model.edit { _ = $0.addRule(group: row.key) } }
                        if row.canNest { ActionButton(title: L("Add Group"), symbol: "plus", small: true) { model.edit { _ = $0.addGroup(group: row.key) } } }
                        ActionButton(title: L("Remove Group"), symbol: "xmark", small: true) { model.edit { $0.remove(key: row.key) } }
                    }
                    .padding(.leading, T.smartGroupIndent)
                }
            }
        case .kept, .rule:
            SmartRuleRowView(shell: shell, row: row, model: model, compact: ctx.cssWidth <= 720)
        }
    }

    private func heading(_ title: String) -> some View {
        Text(title).font(.navia(T.textSection, .bold)).tracking(-0.01 * T.textSection)
    }

    // MARK: Order

    private func sortOptions(_ field: String, first: Bool) -> [SelectOption] {
        var list = first ? [SelectOption(value: smartSortRandom, label: L("Random"))] : []
        list += smartFieldList.filter(\.sortable).map { SelectOption(value: $0.id, label: L($0.label), group: L($0.group)) }
        // A saved sort field outside the catalog, such as a custom tag, stays offered under its own name.
        if !list.contains(where: { $0.value == field }) { list.insert(SelectOption(value: field, label: field), at: 0) }
        return list
    }

    private func label(_ text: String) -> some View {
        Text(text).font(.navia(T.textSM)).foregroundStyle(T.t2.color).frame(width: T.smartSortLabelW, alignment: .leading)
    }

    private var orderSection: some View {
        let random = model.sorts.first?.field == smartSortRandom
        return VStack(alignment: .leading, spacing: T.px(14)) {
            heading(L("Order"))
            VStack(alignment: .leading, spacing: T.smartRowGap) {
                ForEach(Array(model.sorts.enumerated()), id: \.element.key) { index, sort in
                    HStack(spacing: T.smartRowGap) {
                        label(index == 0 ? L("Sort by") : L("Then by"))
                        GlassSelect(shell: shell, label: index == 0 ? L("Sort field") : L("Next sort field {key}", ["key": "\(sort.key)"]), value: sort.field,
                                    options: sortOptions(sort.field, first: index == 0), width: T.smartSortW) { field in model.edit { $0.setSortField(index: UInt32(index), field: field) } }
                        if sort.field != smartSortRandom {
                            SegmentedChoice(look: .glass, value: sort.desc, options: smartDirections(field: sort.field).map { .init(value: $0.desc, label: L($0.label)) },
                                            accent: theme.accent, label: L("Sort direction")) { desc in model.edit { $0.setSortDesc(index: UInt32(index), desc: desc) } }
                        }
                        if model.sorts.count > 1 {
                            IconButton(symbol: "xmark", label: L("Remove sort field"), size: T.px(14), width: T.controlH, height: T.controlH) { model.edit { $0.removeSort(index: UInt32(index)) } }.glassPill()
                        }
                    }
                }
                if let at = model.albumHintAt {
                    HStack(spacing: T.smartRowGap) {
                        Image(systemName: "lightbulb.fill").font(.system(size: T.px(13)))
                        Text(L("Songs added together share a date. Add Album next to keep albums together."))
                        ActionButton(title: L("Add Album"), small: true) { model.edit { $0.addSort(index: at + 1, field: "album") } }
                    }
                    .font(.navia(T.textSM))
                    .foregroundStyle(T.t2.color)
                }
                if !random {
                    ActionButton(title: L("Add Sort Field"), symbol: "plus") { model.edit { $0.addSort(index: UInt32.max, field: nil) } }
                }
            }
            HStack(spacing: T.smartRowGap) {
                label(L("Limit"))
                HStack(spacing: T.px(6)) {
                    Text(L("Top")).font(.navia(T.textSM)).foregroundStyle(T.t2.color)
                    TextField("", text: Binding(get: { model.limit }, set: { value in model.edit { $0.setLimit(text: value) } }), prompt: Text(L("all")).foregroundStyle(T.t3.color))
                        .textFieldStyle(.plain)
                        .font(.navia(T.textMD))
                }
                .padding(.horizontal, T.px(16))
                .frame(width: T.smartLimitW, height: T.controlH)
                .glassPill()
                SegmentedChoice(look: .glass, value: model.limitPercent, options: [.init(value: false, label: L("songs")), .init(value: true, label: L("% of matches"))],
                                accent: theme.accent, label: L("Limit by")) { percent in model.edit { $0.setLimitPercent(percent: percent) } }
            }
        }
    }

    // MARK: Preview

    private var countLine: String {
        let p = model.preview
        guard model.hasRules, p.checkable else { return "" }
        let vars = ["sampled": fmtInt(Int(p.sampled)), "shown": fmtInt(model.songs.count)]
        if model.songs.count < Int(p.matched) {
            return Ln("showing {shown} of {count} match in {sampled} songs on this device", "showing {shown} of {count} matches in {sampled} songs on this device", Int(p.matched), vars)
        }
        return Ln("{count} match in {sampled} songs on this device", "{count} matches in {sampled} songs on this device", Int(p.matched), vars)
    }

    private var caveats: [String] {
        var notes: [String] = []
        let p = model.preview
        var parts: [String] = []
        if !p.unchecked.isEmpty { parts.append(L("Not in the local library, so not applied here: {fields}.", ["fields": p.unchecked.map { L($0) }.joined(separator: ", ")])) }
        if !p.sortUnchecked.isEmpty, model.hasRules {
            parts.append(L("Sorting by {field} isn't applied here.", ["field": p.sortUnchecked.map { L($0) }.joined(separator: ", ")]))
        }
        if !parts.isEmpty { notes.append(parts.joined(separator: " ")) }
        if let existing {
            notes.append(shell.library.smartRulesJSON(playlistID: existing.id) != nil
                ? L("This preview only checks the songs stored on this device, so it can differ from the playlist's real song count.")
                : L("This playlist's rules haven't loaded from the server yet."))
        }
        return notes
    }

    private var previewSection: some View {
        VStack(alignment: .leading, spacing: T.px(14)) {
            HStack(spacing: T.px(16)) {
                heading(L("Preview"))
                Text(countLine).font(.navia(T.textSM)).foregroundStyle(T.t2.color)
            }
            ForEach(caveats, id: \.self) { text in
                HStack(alignment: .top, spacing: T.px(8)) {
                    Image(systemName: "bolt.fill").font(.system(size: T.px(12))).foregroundStyle(T.t2.color).padding(.top, T.px(2))
                    Text(text).font(.navia(T.textSM)).foregroundStyle(T.t2.color).fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

    private var emptyLine: String {
        if model.loading { return L("Checking your library…") }
        if !model.hasRules { return L("Add a rule to see matching songs.") }
        if !model.preview.checkable {
            return model.complete ? L("None of these rules can be checked against the local library.") : L("Finish a rule to see matching songs.")
        }
        return L("No songs here match these rules.")
    }

    @ViewBuilder private var previewList: some View {
        if model.songs.isEmpty {
            Text(emptyLine).font(.navia(T.textMD)).foregroundStyle(T.t2.color)
        } else {
            SongColumnsView(songs: model.songs, shell: shell, one: true, gutter: 0)
        }
    }
}

/// A group's header row, as tall as a rule: All/Any as one sentence, then Add and remove, shown on hover so they
/// don't repeat down every level.
private struct SmartGroupHead: View {
    let shell: ShellModel
    let row: SmartRowView
    let model: SmartEditorModel
    @State private var hovering = false

    var body: some View {
        HStack(spacing: T.smartRowGap) {
            GlassSelect(shell: shell, label: L("Match {key}", ["key": "\(row.key)"]), value: row.any ? "any" : "all",
                        options: [SelectOption(value: "all", label: L("All of the following are true")), SelectOption(value: "any", label: L("Any of the following are true"))]) { value in
                model.edit { $0.setAny(group: row.key, any: value == "any") }
            }
            .fixedSize()
            Spacer(minLength: 0)
            HStack(spacing: T.smartRowGap) {
                MenuIconButton(symbol: "plus", label: L("Add to group"), size: T.px(14), width: T.controlH, height: T.controlH) {
                    var items = [MenuEntry(title: L("Add Rule")) { model.edit { _ = $0.addRule(group: row.key) } }]
                    if row.canNest { items.append(MenuEntry(title: L("Add Group")) { model.edit { _ = $0.addGroup(group: row.key) } }) }
                    return items
                }
                .glassPill()
                IconButton(symbol: "xmark", label: L("Remove group"), size: T.px(14), width: T.controlH, height: T.controlH) { model.edit { $0.remove(key: row.key) } }.glassPill()
            }
            .opacity(hovering || HoverDebug.forced("SmartGroup") ? 1 : 0)
        }
        .frame(height: T.controlH)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
    }
}

/// A rule the editor can't show gets a read-only row.
private struct SmartRuleRowView: View {
    let shell: ShellModel
    let row: SmartRowView
    let model: SmartEditorModel
    let compact: Bool

    private var theme: Theme { shell.theme }

    var body: some View {
        if row.kind == .kept {
            HStack(spacing: T.smartRowGap) {
                HStack(alignment: .firstTextBaseline, spacing: T.px(10)) {
                    Text(L(row.reason)).font(.navia(T.textXS, .semibold)).foregroundStyle(T.t2.color)
                    Text(row.text).font(.navia(T.textSM)).fixedSize(horizontal: false, vertical: true).frame(maxWidth: .infinity, alignment: .leading)
                }
                .padding(.vertical, T.px(9))
                .padding(.horizontal, T.px(16))
                .frame(maxWidth: .infinity, minHeight: T.controlH, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: T.rMD, style: .continuous).fill(T.row.color))
                .overlay(RoundedRectangle(cornerRadius: T.rMD, style: .continuous).strokeBorder(T.line.color, lineWidth: 1))
                removeButton
            }
        } else {
            simple
        }
    }

    private var removeButton: some View {
        IconButton(symbol: "xmark", label: L("Remove rule"), size: T.px(14), width: T.controlH, height: T.controlH) { model.edit { $0.remove(key: row.key) } }.glassPill()
    }

    private var field: SmartFieldRow? { smartFieldList.first { $0.id == row.field } }

    @ViewBuilder private var simple: some View {
        let key = row.key
        let fieldPill = GlassSelect(shell: shell, label: L("Field {key}", ["key": "\(key)"]), value: row.field,
                                    options: smartFieldList.map { SelectOption(value: $0.id, label: L($0.label), group: L($0.group)) }, width: compact ? nil : T.smartFieldW) { id in model.edit { $0.setField(key: key, field: id) } }
        let opPill = GlassSelect(shell: shell, label: L("Condition {key}", ["key": "\(key)"]), value: row.op,
                                 options: smartOps(field: row.field).map { SelectOption(value: $0, label: L(smartOpLabel(op: $0))) }, width: compact ? nil : T.smartOpW) { op in model.edit { $0.setOp(key: key, op: op) } }
        if compact {
            VStack(spacing: T.smartRowGap) {
                HStack(spacing: T.smartRowGap) { fieldPill; opPill; removeButton }
                if row.shape != .none { valueControl.frame(maxWidth: .infinity, alignment: .leading) }
            }
        } else {
            HStack(spacing: T.smartRowGap) {
                fieldPill
                opPill
                Group {
                    if row.shape == .none { Color.clear.frame(height: 1) } else { valueControl }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                removeButton
            }
        }
    }

    @ViewBuilder private var valueControl: some View {
        let key = row.key
        let label = L(field?.label ?? row.field)
        switch row.shape {
        case .bool:
            SegmentedChoice(look: .glass, value: row.flag, options: [.init(value: true, label: L("Yes")), .init(value: false, label: L("No"))], accent: theme.accent, label: L("{field} value", ["field": label])) { on in model.edit { $0.setFlag(key: key, on: on) } }
        case .genre:
            let options = shell.library.genres.map { SelectOption(value: $0, label: $0) }
            GlassSelect(shell: shell, label: L("{field} value {key}", ["field": label, "key": "\(key)"]), placeholder: L("Choose a genre"), value: row.value,
                        options: row.value.isEmpty || options.contains(where: { $0.value == row.value }) ? options : [SelectOption(value: row.value, label: row.value)] + options) { v in model.edit { $0.setChoice(key: key, value: v) } }
        case .playlist:
            let options = shell.library.playlists.map { SelectOption(value: $0.id, label: $0.name) }
            GlassSelect(shell: shell, label: L("{field} value {key}", ["field": label, "key": "\(key)"]), placeholder: L("Choose a playlist"), value: row.value,
                        options: row.value.isEmpty || options.contains(where: { $0.value == row.value }) ? options : [SelectOption(value: row.value, label: L("Playlist {id}", ["id": String(row.value.prefix(8))]))] + options) { v in model.edit { $0.setChoice(key: key, value: v) } }
        case .rangeNumber, .rangeYear, .rangeDate:
            HStack(spacing: T.px(6)) {
                valueInput(value: Binding(get: { row.value }, set: { v in model.edit { $0.setValue(key: key, side: 0, text: v) } }))
                Text(L("to")).font(.navia(T.textSM)).foregroundStyle(T.t2.color)
                valueInput(value: Binding(get: { row.valueTo }, set: { v in model.edit { $0.setValue(key: key, side: 1, text: v) } }), unit: field?.unit.map { L($0) })
            }
            .padding(.leading, T.px(4))
            .padding(.trailing, T.px(12))
            .frame(height: T.controlH)
            .glassPill()
            .invalidRing(row.invalid)
        case .none:
            EmptyView()
        default:
            valueInput(hint: field?.hint.map { L($0) }, value: Binding(get: { row.value }, set: { v in model.edit { $0.setValue(key: key, side: 0, text: v) } }), unit: row.shape == .days ? L("days") : field?.unit.map { L($0) })
                .padding(.horizontal, T.px(16))
                .frame(height: T.controlH)
                .frame(maxWidth: .infinity)
                .glassPill()
                .invalidRing(row.invalid)
        }
    }

    private func valueInput(hint: String? = nil, value: Binding<String>, unit: String? = nil) -> some View {
        HStack(spacing: T.px(6)) {
            TextField("", text: value, prompt: Text(hint ?? L(smartValuePlaceholder(field: row.field, op: row.op))).foregroundStyle(T.t3.color))
                .textFieldStyle(.plain)
                .font(.navia(T.textMD))
            if let unit { Text(unit).font(.navia(T.textSM)).foregroundStyle(T.t2.color).padding(.trailing, T.px(2)) }
        }
    }
}
