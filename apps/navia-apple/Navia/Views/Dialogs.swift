import SwiftUI

struct ConfirmRequest: Identifiable {
    let id = UUID()
    let title: String
    let message: String
    let confirmTitle: String
    /// Uses the smaller Settings buttons, as Sign Out does.
    var compact = false
    let onConfirm: () -> Void
}

/// A modal panel over the whole app, with a scrim that closes it and a titled panel. Every dialog uses this.
struct DialogLayer<Content: View>: View {
    let title: String
    let dismiss: () -> Void
    @ViewBuilder var content: Content

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: T.rLG, style: .continuous)
        GeometryReader { geo in
            ZStack {
                T.scrim.color
                    .contentShape(Rectangle())
                    .onTapGesture(perform: dismiss)
                VStack(alignment: .leading, spacing: T.dialogGap) {
                    Text(title).font(.navia(T.textSection, .semibold)).tracking(-0.012 * T.textSection).foregroundStyle(T.t1.color)
                    content
                }
                .padding(T.dialogPad)
                .frame(width: min(T.dialogW, geo.size.width - T.px(40)), alignment: .leading)
                .dialogPanel(shape)
            }
            .frame(width: geo.size.width, height: geo.size.height)
        }
    }
}

extension View {
    /// The dialog's panel, with its edge and shadow. Taps inside stay inside. The platform glass lightens what it
    /// shows, so the dialog's own color is laid inside it to keep the panel dark.
    func dialogPanel(_ shape: RoundedRectangle) -> some View {
        self
            .plainGlass(shape, flat: T.floatGlass)
            .shadow(T.shadowFloat)
            .contentShape(Rectangle())
            .onTapGesture {}
    }
}

/// The sheet variant, `T.sheetW` wide and up to `T.sheetMaxH` tall. It has a head, a scrolling body, and actions
/// under a full-width hairline.
struct DialogSheet<Head: View, Content: View, Actions: View>: View {
    let dismiss: () -> Void
    @ViewBuilder var head: Head
    @ViewBuilder var content: Content
    @ViewBuilder var actions: Actions
    /// Measured heights, so a short sheet fits its rows and a long one stops at `T.sheetMaxH` and scrolls.
    @State private var bodyHeight: CGFloat = 0
    @State private var chromeHeight: CGFloat = 0

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: T.rLG, style: .continuous)
        GeometryReader { geo in
            let tallest = min(T.sheetMaxH, geo.size.height - T.px(40))
            let room = max(T.px(80), tallest - chromeHeight - 2 * T.dialogGap - T.dialogPad * 2 + T.dialogActionsTop)
            ZStack {
                T.scrim.color
                    .contentShape(Rectangle())
                    .onTapGesture(perform: dismiss)
                VStack(alignment: .leading, spacing: T.dialogGap) {
                    head.onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { headHeight = $0 }
                    ScrollView(.vertical) {
                        content.padding(.bottom, T.dialogGap).frame(maxWidth: .infinity, alignment: .leading)
                            .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { bodyHeight = $0 }
                    }
                    .frame(height: min(bodyHeight, room))
                    .padding(.horizontal, -T.dialogPad)
                    .contentMargins(.horizontal, T.dialogPad, for: .scrollContent)
                    VStack(spacing: 0) {
                        Rectangle().fill(T.line.color).frame(height: 1).padding(.horizontal, -T.dialogPad)
                        HStack(spacing: T.dialogActionsGap) {
                            Spacer(minLength: 0)
                            actions
                        }
                        .padding(.top, T.dialogGap)
                    }
                    .padding(.top, -T.dialogGap)
                    .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { actionsHeight = $0 }
                }
                .padding(.top, T.dialogPad)
                .padding(.horizontal, T.dialogPad)
                .padding(.bottom, T.dialogPad - T.dialogActionsTop)
                .frame(width: min(T.sheetW, geo.size.width - T.px(40)), alignment: .leading)
                .dialogPanel(shape)
            }
            .frame(width: geo.size.width, height: geo.size.height)
        }
        .onChange(of: headHeight + actionsHeight) { _, total in chromeHeight = total }
    }

    @State private var headHeight: CGFloat = 0
    @State private var actionsHeight: CGFloat = 0
}

/// The cover variant, used for an artist's About. The picture runs to the edges at the top and fades into the
/// glass, with the text overlapping the fade, and it all scrolls together. A round close button floats at the top
/// left. Without a picture the text starts the sheet, clear of the close button.
struct DialogCoverSheet<Content: View>: View {
    /// fills the band at the top, already cropped to its focus.
    let art: AnyView?
    let dismiss: () -> Void
    @ViewBuilder var content: Content
    @State private var bodyHeight: CGFloat = 0

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: T.rLG, style: .continuous)
        GeometryReader { geo in
            ZStack {
                T.scrim.color
                    .contentShape(Rectangle())
                    .onTapGesture(perform: dismiss)
                ZStack(alignment: .topLeading) {
                    ScrollView(.vertical) {
                        VStack(alignment: .leading, spacing: 0) {
                            if let art {
                                art
                                    .frame(height: T.sheetCoverH)
                                    .mask(LinearGradient(stops: [.init(color: .black, location: T.sheetCoverFadeFrom), .init(color: .clear, location: 1)], startPoint: .top, endPoint: .bottom))
                                    .padding(.bottom, -T.sheetCoverOverlap)
                            }
                            content
                                .padding(.horizontal, T.dialogPad)
                                .padding(.top, art == nil ? T.dialogGap : 0)
                                .padding(.bottom, T.dialogPad)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { bodyHeight = $0 }
                    }
                    .scrollIndicators(.never)
                    .frame(height: min(bodyHeight, min(T.sheetMaxH, geo.size.height - T.px(40))))
                    IconButton(symbol: "xmark", label: L("Close"), width: T.controlH, height: T.controlH, action: dismiss)
                        .glassPill()
                        .padding(T.dialogGap)
                }
                .frame(width: min(T.sheetW, geo.size.width - T.px(40)), alignment: .leading)
                .clipShape(shape)
                .dialogPanel(shape)
            }
            .frame(width: geo.size.width, height: geo.size.height)
        }
    }
}

enum CoverSheet {
    /// How far a picture-less cover sheet's first line stays clear of the close button.
    static var headInset: CGFloat { T.dialogGap * 2 + T.controlH - T.dialogPad }
}

struct DialogText: View {
    let text: String
    var body: some View {
        Text(text).font(.navia(T.textMD)).foregroundStyle(T.t2.color).lineSpacing(T.textMD * 0.4).fixedSize(horizontal: false, vertical: true)
    }
}

/// A dialog's small print. `error` marks a wrong value, such as a code.
struct DialogNote: View {
    enum Tone { case plain, error }
    let text: String
    var tone: Tone = .plain
    var body: some View {
        Text(text).font(.navia(T.textSM)).foregroundStyle(tone == .error ? T.dangerInk.color : T.t3.color).lineSpacing(T.textSM * 0.4).fixedSize(horizontal: false, vertical: true)
            .accessibilityAddTraits(tone == .error ? .updatesFrequently : [])
    }
}

/// A short code typed digit by digit, such as an AirPlay PIN. One box per digit, with the active one ringed and
/// all ringed when `invalid`. A hidden field takes typing and pasting, and keeps only digits.
struct DialogPin: View {
    let label: String
    @Binding var value: String
    var length = 4
    var invalid = false
    var disabled = false
    let accent: RGBA
    var onSubmit: () -> Void = {}
    @FocusState private var focused: Bool

    var body: some View {
        ZStack {
            TextField("", text: $value)
                .textFieldStyle(.plain)
                .focused($focused)
                .onSubmit(onSubmit)
                .opacity(0.011)
                .accessibilityLabel(label)
                .disabled(disabled)
            HStack(spacing: T.pinGap) {
                ForEach(0..<length, id: \.self) { i in box(i) }
            }
            .allowsHitTesting(false)
        }
        .frame(maxWidth: .infinity)
        .contentShape(Rectangle())
        .onTapGesture { focused = true }
        .onChange(of: value) { _, now in
            let digits = String(now.filter(\.isNumber).prefix(length))
            if digits != now { value = digits }
        }
        .task {
            try? await Task.sleep(for: .milliseconds(150))
            focused = true
        }
    }

    private func box(_ i: Int) -> some View {
        let chars = Array(value)
        let shape = RoundedRectangle(cornerRadius: T.rMD, style: .continuous)
        let current = focused && !disabled && i == min(chars.count, length - 1)
        return Text(i < chars.count ? String(chars[i]) : "")
            .font(.navia(T.pinText, .semibold).monospacedDigit())
            .foregroundStyle(T.t1.color)
            .frame(width: T.pinBoxW, height: T.pinBoxH)
            .background(shape.fill(T.glass.color))
            .overlay(shape.strokeBorder(T.glassEdge.color, lineWidth: 1))
            .overlay(shape.strokeBorder((invalid ? T.invalidRing : accent).color, lineWidth: invalid ? T.invalidRingW : T.focusRingWidth).padding(invalid ? -T.invalidRingW : -T.focusRingWidth).opacity(invalid || current ? 1 : 0))
            .opacity(disabled ? T.disabledOpacity : 1)
    }
}

struct DialogSwitch: View {
    let label: String
    let hint: String
    @Binding var isOn: Bool
    let accent: RGBA
    var body: some View {
        HStack(spacing: T.px(16)) {
            VStack(alignment: .leading, spacing: 1) {
                Text(label).font(.navia(T.textMD, .medium)).foregroundStyle(T.t1.color)
                Text(hint).font(.navia(T.textSM)).foregroundStyle(T.t2.color).fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
            ToggleSwitch(isOn: $isOn, accent: accent, label: label)
        }
    }
}

struct DialogBlock<Content: View>: View {
    let label: String
    @ViewBuilder var content: Content
    var body: some View {
        VStack(alignment: .leading, spacing: T.px(6)) {
            Text(label).font(.navia(T.textSM)).foregroundStyle(T.t2.color)
            content
        }
    }
}

struct DialogActions<Content: View>: View {
    @ViewBuilder var content: Content
    var body: some View {
        HStack(spacing: T.dialogActionsGap) {
            Spacer(minLength: 0)
            content
        }
        .padding(.top, T.dialogActionsTop)
    }
}

struct ConfirmLayer: View {
    let request: ConfirmRequest
    let dismiss: () -> Void

    var body: some View {
        DialogLayer(title: request.title, dismiss: dismiss) {
            DialogText(text: request.message)
            DialogActions {
                ActionButton(title: L("Cancel"), compact: request.compact, action: dismiss)
                ActionButton(title: request.confirmTitle, kind: .prominent(tint: T.danger), compact: request.compact) {
                    dismiss()
                    request.onConfirm()
                }
            }
        }
    }
}

struct ErrorText: View {
    let text: String
    var body: some View {
        Text(text).font(.navia(T.textSM)).foregroundStyle(T.danger.color).fixedSize(horizontal: false, vertical: true)
    }
}

/// The New Playlist dialog. From Add to Playlist it starts with a name and holds those songs. From the smart
/// playlist editor's Save As it names a copy of the rules.
struct NewPlaylistLayer: View {
    let shell: ShellModel
    @State private var name: String
    private let songs: [SongRow]?
    private let smartCriteria: String?

    init(shell: ShellModel) {
        self.shell = shell
        _name = State(initialValue: shell.newPlaylistSeed?.name ?? "")
        smartCriteria = shell.smartSaveAs
        songs = smartCriteria == nil ? shell.newPlaylistSeed.flatMap { $0.songs.isEmpty ? nil : $0.songs } : nil
        covers = smartCriteria == nil && shell.library.canChangePlaylistImages
        _slot = State(initialValue: shell.coverSlotSeed ?? Self.slots.upload + 1)
    }
    private static let slots = CoverSlots(current: false)
    private let covers: Bool
    @State private var slot: Int
    @State private var picked: URL?
    @State private var description = ""
    @State private var isPublic = false
    @State private var creating = false
    @State private var problem: String?

    private var canCreate: Bool { !name.trimmingCharacters(in: .whitespaces).isEmpty && !creating }

    var body: some View {
        DialogLayer(title: smartCriteria == nil ? L("New Playlist") : L("Save As"), dismiss: { shell.closeNewPlaylist() }) {
            if smartCriteria != nil { DialogText(text: L("Saves these rules as a new smart playlist of your own. The original stays as it is.")) }
            if covers {
                CoverPicker(title: name.trimmingCharacters(in: .whitespaces).isEmpty ? L("Playlist Name") : name.trimmingCharacters(in: .whitespaces), slot: $slot, picked: $picked, accent: shell.theme.accent)
                    .padding(.horizontal, -T.dialogPad)
            }
            DialogBlock(label: L("Name")) {
                PillTextField(placeholder: L("Playlist Name"), text: $name, accent: shell.theme.accent, onSubmit: create, onEscape: { shell.closeNewPlaylist() }, autoFocus: true)
            }
            if covers {
                DialogBlock(label: L("Description")) {
                    PillTextField(placeholder: L("Optional"), text: $description, accent: shell.theme.accent, onSubmit: create, onEscape: { shell.closeNewPlaylist() })
                }
                DialogSwitch(label: L("Public"), hint: L("Other people on this server can see it."), isOn: $isPublic, accent: shell.theme.accent)
            }
            if let problem { ErrorText(text: problem) }
            DialogActions {
                ActionButton(title: L("Cancel"), enabled: !creating) { shell.closeNewPlaylist() }
                ActionButton(title: smartCriteria == nil ? (creating ? L("Creating…") : L("Create")) : (creating ? L("Saving…") : L("Save")), kind: .prominent(tint: shell.theme.accent), enabled: canCreate, action: create)
            }
        }
        .onChange(of: shell.dialogSubmit) { create() }
    }

    private func create() {
        let title = name.trimmingCharacters(in: .whitespaces)
        guard !title.isEmpty, !creating else { return }
        creating = true
        problem = nil
        if let criteria = smartCriteria {
            Task { @MainActor in
                switch await shell.library.saveSmartPlaylist(id: nil, name: title, criteria: criteria) {
                case .success(let id):
                    shell.closeNewPlaylist()
                    shell.go(.playlist(id))
                    shell.showToast(L("Created {name}", ["name": title]))
                case .failure(let failure):
                    creating = false
                    problem = failure.message
                }
            }
            return
        }
        Task { @MainActor in
            do {
                let id = try await shell.library.createPlaylist(name: title, songs: songs ?? [])
                let trouble = covers && id != nil ? await finish(id!, title: title) : nil
                shell.closeNewPlaylist()
                if let trouble { shell.showToast(trouble); if songs == nil, let id { shell.go(.playlist(id)) }; return }
                if songs != nil {
                    shell.showToast(L("Added to {name}", ["name": title]))
                    return
                }
                if let id { shell.go(.playlist(id)) }
                shell.showToast(L("Created {name}", ["name": title]))
            } catch {
                creating = false
                problem = (error as? LocalizedError)?.errorDescription ?? "\(error)"
            }
        }
    }
}

extension NewPlaylistLayer {
    /// after the playlist exists: its description and Public, then its cover. returns what the server refused.
    fileprivate func finish(_ id: String, title: String) async -> String? {
        let comment = description.trimmingCharacters(in: .whitespaces)
        var trouble = await shell.library.updatePlaylistDetails(id: id, name: nil, comment: comment.isEmpty ? nil : comment, isPublic: isPublic)
        if let problem = await uploadChosenCover(slot: slot, slots: Self.slots, picked: picked, title: title, playlist: id, library: shell.library) { trouble = problem }
        return trouble
    }
}

/// Edit Details for an owned playlist; only what changed is sent.
struct EditPlaylistLayer: View {
    let playlist: PlaylistRow
    let shell: ShellModel
    @State private var name: String
    @State private var description: String
    @State private var isPublic: Bool
    @State private var slot: Int
    @State private var picked: URL?
    @State private var saving = false
    @State private var problem: String?
    private static let slots = CoverSlots(current: true)
    private let covers: Bool

    init(playlist: PlaylistRow, shell: ShellModel) {
        self.playlist = playlist
        self.shell = shell
        _name = State(initialValue: playlist.name)
        _description = State(initialValue: playlist.summary)
        _isPublic = State(initialValue: shell.isPublic(playlist))
        covers = playlist.isOwned && shell.library.canChangePlaylistImages
        _slot = State(initialValue: shell.coverSlotSeed ?? 0)
    }

    private var trimmedName: String { name.trimmingCharacters(in: .whitespaces) }
    private var canSave: Bool { !trimmedName.isEmpty && !saving }
    /// another cover than the current one, with a picture to upload behind it.
    private var newCover: Bool { covers && !Self.slots.isCurrent(slot) && (slot != Self.slots.upload || picked != nil) }

    var body: some View {
        DialogLayer(title: L("Edit Details"), dismiss: { shell.closeEditPlaylist() }) {
            if covers {
                CoverPicker(title: trimmedName.isEmpty ? L("Playlist Name") : trimmedName, current: playlist, slot: $slot, picked: $picked, accent: shell.theme.accent)
                    .padding(.horizontal, -T.dialogPad)
            }
            DialogBlock(label: L("Name")) {
                PillTextField(placeholder: "", text: $name, accent: shell.theme.accent, onSubmit: save, onEscape: { shell.closeEditPlaylist() }, autoFocus: true)
            }
            DialogBlock(label: L("Description")) {
                PillTextField(placeholder: L("Optional"), text: $description, accent: shell.theme.accent, onSubmit: save, onEscape: { shell.closeEditPlaylist() })
            }
            DialogSwitch(label: L("Public"), hint: L("Other people on this server can see it."), isOn: $isPublic, accent: shell.theme.accent)
            if let problem { ErrorText(text: problem) }
            DialogActions {
                ActionButton(title: L("Cancel"), enabled: !saving) { shell.closeEditPlaylist() }
                ActionButton(title: saving ? L("Saving…") : L("Save"), kind: .prominent(tint: shell.theme.accent), enabled: canSave, action: save)
            }
        }
        .onChange(of: shell.dialogSubmit) { save() }
    }

    private func save() {
        guard canSave else { return }
        let text = description.trimmingCharacters(in: .whitespaces)
        let newName = trimmedName != playlist.name ? trimmedName : nil
        let newText = text != playlist.summary ? text : nil
        let newPublic = isPublic != shell.isPublic(playlist) ? isPublic : nil
        let details = newName != nil || newText != nil || newPublic != nil
        if !details, !newCover {
            shell.closeEditPlaylist()
            return
        }
        saving = true
        problem = nil
        Task { @MainActor in
            var failed = details ? await shell.library.updatePlaylistDetails(id: playlist.id, name: newName, comment: newText, isPublic: newPublic) : nil
            if failed == nil, newCover {
                failed = await uploadChosenCover(slot: slot, slots: Self.slots, picked: picked, title: trimmedName, playlist: playlist.id, library: shell.library)
            }
            if let failed {
                saving = false
                problem = failed
            } else {
                shell.closeEditPlaylist()
            }
        }
    }
}

/// Songs about to be added to a playlist that already holds some of them.
struct DuplicatesRequest: Identifiable {
    let id = UUID()
    let target: PlaylistRef
    let songs: [SongRow]
    let fresh: [SongRow]
}

/// "Already in <playlist>". If every song is a duplicate it offers Cancel or Add Anyway. If only some are, it
/// offers Add Anyway or Skip Duplicates.
struct DuplicatesLayer: View {
    let request: DuplicatesRequest
    let shell: ShellModel

    var body: some View {
        DialogLayer(title: L("Already in {name}", ["name": request.target.name]), dismiss: { shell.closeDuplicates() }) {
            DialogText(text: Self.text(request))
            DialogActions {
                if request.fresh.isEmpty {
                    ActionButton(title: L("Cancel")) { shell.closeDuplicates() }
                    ActionButton(title: L("Add Anyway"), kind: .prominent(tint: shell.theme.accent)) { shell.answerDuplicates(request, skip: false) }
                } else {
                    ActionButton(title: L("Add Anyway")) { shell.answerDuplicates(request, skip: false) }
                    ActionButton(title: L("Skip Duplicates"), kind: .prominent(tint: shell.theme.accent)) { shell.answerDuplicates(request, skip: true) }
                }
            }
        }
    }

    static func text(_ request: DuplicatesRequest) -> String {
        let name = request.target.name
        if request.songs.count == 1 { return L("“{song}” is already in this playlist.", ["song": request.songs[0].title]) }
        if request.fresh.isEmpty { return Ln("The song is already in {name}.", "All {count} songs are already in {name}.", request.songs.count, ["name": name]) }
        return Ln("{count} of {total} songs is already in {name}.", "{count} of {total} songs are already in {name}.", request.songs.count - request.fresh.count,
                  ["total": fmtInt(request.songs.count), "name": name])
    }
}
