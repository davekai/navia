import AppKit
import NaviaCoreFFI
import SwiftUI

enum InfoTarget: Equatable {
    case song(String)
    case album(String)
    case playlist(String)
    case artist(String)
}

/// Get Info, showing everything the server says about a song, album, playlist or artist. A row shows only when the
/// server sent its field, and empty sections are left out.
struct GetInfoLayer: View {
    let target: InfoTarget
    let shell: ShellModel
    @State private var copied: String?

    private var core: NaviaCore? { (shell.library as? CoreLibraryModel)?.core }

    var body: some View {
        if let core {
            DialogSheet(dismiss: { shell.closeInfo() }) {
                head(core)
            } content: {
                VStack(alignment: .leading, spacing: T.infoSectionGap) { sections(core) }
            } actions: {
                ActionButton(title: L("Done"), kind: .prominent(tint: shell.theme.accent)) { shell.closeInfo() }
            }
        }
    }

    // MARK: Head

    @ViewBuilder private func head(_ core: NaviaCore) -> some View {
        switch target {
        case .song(let id):
            if let s = core.songDetails(id: id) {
                let album = s.albumId.flatMap { shell.library.album(id: $0) }
                headView(kind: L("Song"), title: s.title, art: ArtworkView(url: album?.artworkURL, pixels: 300).clipShape(RoundedRectangle(cornerRadius: T.rMD, style: .continuous))) {
                    LinkedText(pieces: [LinkPiece(id: 0, text: s.artist ?? "", route: s.artistId.map { .artist($0) })]
                        + (s.album.map { [LinkPiece(id: 1, text: " — ", route: nil), LinkPiece(id: 2, text: $0, route: s.albumId.map { .album($0) })] } ?? []), shell: shell)
                }
            }
        case .album(let id):
            if let a = core.albumDetails(id: id) {
                headView(kind: L("Album"), title: a.name, art: ArtworkView(url: shell.library.album(id: id)?.artworkURL, pixels: 300).clipShape(RoundedRectangle(cornerRadius: T.rMD, style: .continuous))) {
                    LinkedText(pieces: [LinkPiece(id: 0, text: a.artist ?? "", route: a.artistId.map { .artist($0) })], shell: shell)
                }
            }
        case .playlist(let id):
            if let p = core.playlistDetails(id: id) {
                let row = shell.library.playlist(id: id)
                headView(kind: p.smart ? L("Smart Playlist") : L("Playlist"), title: p.name, art: Group { if let row { PlaylistCover(playlist: row) } }) {
                    if let owner = p.owner { Text(L("By {name}", ["name": owner])) }
                }
            }
        case .artist(let id):
            if let a = core.artistDetails(id: id) {
                headView(kind: L("Artist"), title: a.name, art: ArtworkView(url: shell.library.artist(id: id)?.photoURL, kind: .artist, pixels: 300).clipShape(Circle())) { EmptyView() }
            }
        }
    }

    private func headView(kind: String, title: String, art: some View, @ViewBuilder sub: () -> some View) -> some View {
        HStack(spacing: T.px(16)) {
            art.frame(width: T.infoArt, height: T.infoArt)
            VStack(alignment: .leading, spacing: T.px(2)) {
                Text(kind).font(.navia(T.textXS, .semibold)).foregroundStyle(T.tSection.color)
                Text(title).font(.navia(T.textFeature, .bold)).foregroundStyle(T.tEm.color).lineLimit(3).fullTextHelp(title)
                sub().font(.navia(T.textMD)).foregroundStyle(T.t2.color)
            }
        }
    }

    // MARK: Sections

    @ViewBuilder private func sections(_ core: NaviaCore) -> some View {
        switch target {
        case .song(let id): if let s = core.songDetails(id: id) { songSections(s, core: core) }
        case .album(let id): if let a = core.albumDetails(id: id) { albumSections(a) }
        case .playlist(let id): if let p = core.playlistDetails(id: id) { playlistSections(p) }
        case .artist(let id): if let a = core.artistDetails(id: id) { artistSections(a) }
        }
    }

    private func songSections(_ s: SongDetailsRow, core: NaviaCore) -> some View {
        let album = s.albumId.flatMap { core.albumDetails(id: $0) }
        let discs = album?.discCount.map(Int.init)
        let released = album?.releaseDate.flatMap(Self.release)
        let original = album?.originalReleaseDate.flatMap(Self.release)
        var song: [InfoField] = [.text(L("Title"), s.title)]
        if !s.artists.isEmpty {
            song.append(.links(s.artists.count > 1 ? L("Artists") : L("Artist"), s.artists.map { ($0.name, $0.artistId.map { Route.artist($0) }) }))
        } else if let artist = s.artist {
            song.append(.links(L("Artist"), [(artist, s.artistId.map { Route.artist($0) })]))
        }
        for credit in s.credits.filter({ $0.role != "maincredit" }).sorted(by: { Self.roleRank($0.role) < Self.roleRank($1.role) }) {
            song.append(.links(Self.roleName(credit.role, count: credit.names.count),
                               credit.names.map { n in (n.subRole.map { "\(n.name) (\($0))" } ?? n.name, n.artistId.map { Route.artist($0) }) }))
        }
        if let name = s.album { song.append(.links(L("Album"), [(name, s.albumId.map { Route.album($0) })])) }
        if let track = s.track, track > 0 { song.append(.text(L("Track"), fmtInt(Int(track)))) }
        if let disc = s.disc.map(Int.init), disc > 0, discs.map({ $0 > 1 }) ?? (disc > 1) {
            song.append(.text(L("Disc"), discs.map { L("{n} of {total}", ["n": fmtInt(disc), "total": fmtInt($0)]) } ?? fmtInt(disc)))
        }
        if let released { song.append(.text(L("Released"), released)) }
        if let original, original != released { song.append(.text(L("Original Release"), original)) }
        if released == nil, original == nil, let year = s.year, year > 0 { song.append(.text(L("Year"), String(year))) }
        if let genres = genreField(s.genres) { song.append(genres) }
        if s.explicitStatus == "explicit" { song.append(.text(L("Explicit"), L("Yes"))) }

        var file: [InfoField] = []
        if let suffix = s.suffix, !suffix.isEmpty, let format = s.format {
            file.append(.text(L("Format"), format.lowercased() == suffix.lowercased() ? format : "\(format) (\(suffix.uppercased()))"))
            if let kbps = s.bitRateKbps, kbps > 0 { file.append(.text(L("Bitrate"), L("{n} kbps", ["n": fmtInt(Int(kbps))]))) }
            if let depth = s.bitDepth, depth > 0 { file.append(.text(L("Bit Depth"), L("{n}-bit", ["n": String(depth)]))) }
            if let rate = s.samplingRate, rate > 0 { file.append(.text(L("Sample Rate"), AudioQuality.khz(Int(rate)))) }
            if let channels = s.channelCount, channels > 0 { file.append(.text(L("Channels"), AudioQuality.channelsLabel(Int(channels)))) }
            if let size = s.size { file.append(.text(L("Size"), Self.fileSize(size))) }
            if let length = s.durationSeconds { file.append(.text(L("Length"), clock(Double(length)))) }
            if let path = s.path { file.append(.code(L("Path"), path)) }
        }

        let rg = s.replayGain
        var details: [InfoField] = []
        if let bpm = s.bpm, bpm > 0 { details.append(.text(L("BPM"), fmtInt(Int(bpm)))) }
        if let g = Self.gain(rg?.trackGain, rg?.trackPeak) { details.append(.text(L("Track Gain"), g)) }
        if let g = Self.gain(rg?.albumGain, rg?.albumPeak) { details.append(.text(L("Album Gain"), g)) }
        if !s.isrc.isEmpty { details.append(.code("ISRC", s.isrc.joined(separator: ", "))) }
        if let comment = s.comment, !comment.isEmpty { details.append(.text(L("Comment"), comment)) }
        if let mb = s.musicBrainzId { details.append(.code(L("MusicBrainz Recording"), mb)) }

        // Playing Now shows only for the song playing, from the quality badge's data.
        var now: [InfoField] = []
        if shell.player.station == nil, shell.player.current?.id == s.id, let q = shell.player.quality {
            now.append(q.transcodeFormat.map { format in
                InfoField.noted(L("Plays As"), q.transcodeKbps.map { "\(format) · \(L("{n} kbps", ["n": fmtInt($0)]))" } ?? format, L("Transcoded by the server"))
            }
                ?? .text(L("Plays As"), L("The original file")))
            if let source = q.source { now.append(.text(L("From||where a song plays from"), AudioQuality.sourceLabel(source))) }
        }
        let listened = listening(plays: s.playCount, played: s.played, added: s.created, favorite: s.starred != nil)
        return VStack(alignment: .leading, spacing: T.infoSectionGap) {
            section(L("Song"), song)
            section(L("File"), file)
            section(L("Playing Now"), now)
            section(L("Details"), details)
            section(L("Your Listening"), listened)
        }
    }

    private func albumSections(_ a: AlbumDetailsRow) -> some View {
        let released = a.releaseDate.flatMap(Self.release)
        let original = a.originalReleaseDate.flatMap(Self.release)
        var album: [InfoField] = []
        if let artist = a.artist { album.append(.links(L("Artist"), [(artist, a.artistId.map { Route.artist($0) })])) }
        if !a.releaseTypes.isEmpty { album.append(.text(a.releaseTypes.count > 1 ? L("Release Types") : L("Release Type"), a.releaseTypes.map(\.capitalized).joined(separator: ", "))) }
        // The edition label from the core. None for explicit or clean.
        if let version = a.version { album.append(.text(L("Version"), version)) }
        if !a.recordLabels.isEmpty { album.append(.text(a.recordLabels.count > 1 ? L("Record Labels") : L("Record Label"), a.recordLabels.joined(separator: ", "))) }
        if let released { album.append(.text(L("Released"), released)) }
        if let original, original != released { album.append(.text(L("Original Release"), original)) }
        if released == nil, original == nil, let year = a.year, year > 0 { album.append(.text(L("Year"), String(year))) }
        if let genres = genreField(a.genres) { album.append(genres) }
        if a.explicitStatus == "explicit" { album.append(.text(L("Explicit"), L("Yes"))) }
        if a.isCompilation == true { album.append(.text(L("Compilation"), L("Yes"))) }
        var contents: [InfoField] = []
        if let n = a.songCount { contents.append(.text(L("Songs"), fmtInt(Int(n)))) }
        if let d = a.discCount, d > 1 { contents.append(.text(L("Discs"), fmtInt(Int(d)))) }
        if let length = a.durationSeconds, length > 0 { contents.append(.text(L("Length"), longDuration(Int(length)))) }
        if let size = a.size { contents.append(.text(L("Size"), Self.fileSize(size))) }
        let about = shell.library.album(id: a.id)?.blurb ?? ""
        return VStack(alignment: .leading, spacing: T.infoSectionGap) {
            section(L("Album"), album)
            section(L("Contents"), contents)
            section(L("Your Listening"), listening(plays: a.playCount, played: a.played, added: a.created, favorite: a.starred != nil))
            if !about.isEmpty { section(L("About"), [], block: AnyView(InfoParagraph(text: about))) }
            section(L("Identifiers"), a.musicBrainzId.map { [InfoField.code(L("MusicBrainz Release"), $0)] } ?? [])
        }
    }

    private func playlistSections(_ p: PlaylistDetailsRow) -> some View {
        var fields: [InfoField] = []
        if let owner = p.owner { fields.append(.text(L("Owner"), owner)) }
        if let on = p.public { fields.append(.text(L("Visibility"), on ? L("Public") : L("Private"))) }
        if let created = p.created, !fmtDateTime(created).isEmpty { fields.append(.text(L("Created"), fmtDateTime(created))) }
        if let changed = p.changed, !fmtDateTime(changed).isEmpty { fields.append(.text(L("Changed"), fmtDateTime(changed))) }
        if let n = p.songCount { fields.append(.text(L("Songs"), fmtInt(Int(n)))) }
        if let length = p.durationSeconds, length > 0 { fields.append(.text(L("Length"), longDuration(Int(length)))) }
        if let size = p.size { fields.append(.text(L("Size"), Self.fileSize(size))) }
        if let comment = p.comment, !comment.isEmpty { fields.append(.text(L("Comment"), comment)) }
        let rules = shell.library.playlist(id: p.id)?.rules
        return VStack(alignment: .leading, spacing: T.infoSectionGap) {
            section(L("Playlist"), fields)
            if p.smart {
                // The songs stand until `validUntil`. The server runs the rules again the next time the playlist is read after it.
                section(L("Smart Playlist"), p.validUntil.map { until -> [InfoField] in
                    let past = (ISO8601DateFormatter().date(from: until) ?? .distantFuture) < Date()
                    return [past ? .noted(L("Songs Up to Date Until"), fmtDateTime(until), L("The server updates them the next time the playlist is opened.")) : .text(L("Songs Up to Date Until"), fmtDateTime(until))]
                } ?? [], block: AnyView(Group {
                    if let rules, !rules.rules.isEmpty {
                        RuleListView(summary: rules)
                    } else {
                        HStack(spacing: T.px(5)) {
                            Image(systemName: "bolt.fill").font(.system(size: T.px(12))).foregroundStyle(shell.theme.accent.color)
                            Text(L("The rules come from the server and aren’t available here.")).foregroundStyle(T.t2.color)
                        }
                    }
                }))
            }
        }
    }

    private func artistSections(_ a: ArtistDetailsRow) -> some View {
        let roles = a.roles.filter { $0 != "maincredit" }.sorted { Self.roleRank($0) < Self.roleRank($1) }.map { Self.roleName($0, count: 1) }
        var fields: [InfoField] = []
        if let n = a.albumCount { fields.append(.text(L("Albums"), fmtInt(Int(n)))) }
        if a.songCount > 0 { fields.append(.text(L("Songs"), fmtInt(Int(a.songCount)))) }
        if !roles.isEmpty { fields.append(.text(roles.count > 1 ? L("Roles") : L("Role"), roles.joined(separator: ", "))) }
        if a.playCount > 0 { fields.append(.text(L("Plays"), fmtInt(Int(a.playCount)))) }
        fields.append(.text(L("Favorite"), a.starred != nil ? L("Yes") : L("No")))
        if let mb = a.musicBrainzId { fields.append(.code(L("MusicBrainz Artist"), mb)) }
        // The biography is only in the About sheet, so Get Info stays technical.
        return VStack(alignment: .leading, spacing: T.infoSectionGap) {
            section(L("Artist"), fields)
        }
    }

    private func listening(plays: UInt64?, played: String?, added: String?, favorite: Bool) -> [InfoField] {
        var fields: [InfoField] = []
        if let plays { fields.append(.text(L("Plays"), plays > 0 ? fmtInt(Int(plays)) : L("Never played"))) }
        if let played, !fmtDate(played).isEmpty { fields.append(.text(L("Last Played"), fmtDate(played))) }
        if let added, !fmtDate(added).isEmpty { fields.append(.text(L("Added"), fmtDate(added))) }
        fields.append(.text(L("Favorite"), favorite ? L("Yes") : L("No")))
        return fields
    }

    private func genreField(_ genres: [String]) -> InfoField? {
        genres.isEmpty ? nil : .links(genres.count > 1 ? L("Genres") : L("Genre"), genres.map { ($0, Route.songs(genre: $0)) })
    }

    /// A titled group of rows with hairlines between, and an optional block under them.
    @ViewBuilder private func section(_ title: String, _ fields: [InfoField], block: AnyView? = nil) -> some View {
        if !fields.isEmpty || block != nil {
            VStack(alignment: .leading, spacing: T.px(7)) {
                Text(title).font(.navia(T.textSM, .semibold)).foregroundStyle(T.tSection.color).padding(.leading, T.infoRowPadX)
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(fields.enumerated()), id: \.offset) { index, field in
                        if index > 0 { Rectangle().fill(T.line.color).frame(height: 1) }
                        row(field)
                    }
                    if let block {
                        if !fields.isEmpty { Rectangle().fill(T.line.color).frame(height: 1) }
                        block.padding(.horizontal, T.infoRowPadX).padding(.vertical, T.infoRowPadY)
                    }
                }
                .background(T.row.color, in: RoundedRectangle(cornerRadius: T.rMD, style: .continuous))
                .clipShape(RoundedRectangle(cornerRadius: T.rMD, style: .continuous))
            }
        }
    }

    private func row(_ field: InfoField) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: T.px(14)) {
            Text(field.label).foregroundStyle(T.t2.color).frame(width: T.infoLabelW, alignment: .leading)
            HStack(alignment: .top, spacing: T.px(8)) {
                value(field).frame(maxWidth: .infinity, alignment: .leading)
                if let copy = field.copy { copyButton(copy, label: field.label) }
            }
        }
        .font(.navia(T.textMD).monospacedDigit())
        .foregroundStyle(T.t1.color)
        .padding(.horizontal, T.infoRowPadX)
        .padding(.vertical, T.infoRowPadY)
    }

    @ViewBuilder private func value(_ field: InfoField) -> some View {
        switch field.kind {
        case .text(let text): Text(text).fixedSize(horizontal: false, vertical: true).textSelection(.enabled)
        case .code(let text): Text(text).font(.navia(T.textSM)).fixedSize(horizontal: false, vertical: true).textSelection(.enabled)
        case .noted(let text, let note):
            VStack(alignment: .leading, spacing: T.px(1)) {
                Text(text)
                Text(note).font(.navia(T.textSM)).foregroundStyle(T.t2.color).fixedSize(horizontal: false, vertical: true)
            }
        case .links(let items):
            LinkedText(pieces: items.enumerated().flatMap { i, item in
                (i > 0 ? [LinkPiece(id: i * 2 - 1, text: ", ", route: nil)] : []) + [LinkPiece(id: i * 2, text: item.0, route: item.1)]
            }, shell: shell)
        }
    }

    /// A quiet copy button at a value's end. It shows a check briefly once copied.
    private func copyButton(_ text: String, label: String) -> some View {
        Button {
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(text, forType: .string)
            copied = label
            shell.showToast(L("Copied {name}", ["name": label]))
            Task { @MainActor in
                try? await Task.sleep(for: .milliseconds(1500))
                if copied == label { copied = nil }
            }
        } label: {
            Image(systemName: copied == label ? "checkmark" : "doc.on.doc")
                .font(.system(size: T.px(13)))
                .foregroundStyle(T.t2.color)
                .frame(width: T.px(26), height: T.px(26))
                .contentShape(Rectangle())
        }
        .buttonStyle(HoverFillStyle(key: "copy-\(label)"))
        .help(L("Copy {name}", ["name": label]))
        .padding(.vertical, -T.px(3))
        .padding(.trailing, -T.px(6))
    }

    // MARK: Values

    private static func release(_ d: ItemDateRow) -> String? {
        guard let year = d.year, year > 0 else { return nil }
        return fmtRelease(year: Int(year), month: d.month.map(Int.init), day: d.day.map(Int.init))
    }

    private static func signed(_ n: Double) -> String { (n > 0 ? "+" : n < 0 ? "−" : "") + fmtDec(abs(n), 2) }

    private static func gain(_ g: Double?, _ peak: Double?) -> String? {
        guard let g else { return nil }
        if let peak { return L("{gain} dB · peak {peak}", ["gain": signed(g), "peak": fmtDec(peak, 3)]) }
        return L("{gain} dB", ["gain": signed(g)])
    }

    /// A file's size as file browsers show it, such as "34.2 MB".
    static func fileSize(_ bytes: UInt64) -> String {
        let b = Double(bytes)
        if b >= 1e9 { return L("{n} GB", ["n": fmtDec(b / 1e9, 2)]) }
        if b >= 1e6 { return L("{n} MB", ["n": fmtDec(b / 1e6, 1)]) }
        return L("{n} KB", ["n": fmtInt(max(1, Int((b / 1e3).rounded())))])
    }

    // The usual credit order. Other roles the server sends follow in its order.
    private static let roles: [(String, String, String)] = [
        ("artist", "Artist", "Artists"), ("albumartist", "Album Artist", "Album Artists"), ("composer", "Composer", "Composers"),
        ("lyricist", "Lyricist", "Lyricists"), ("producer", "Producer", "Producers"), ("performer", "Performer", "Performers"),
        ("conductor", "Conductor", "Conductors"), ("arranger", "Arranger", "Arrangers"), ("director", "Director", "Directors"),
        ("engineer", "Engineer", "Engineers"), ("mixer", "Mixer", "Mixers"), ("remixer", "Remixer", "Remixers"), ("djmixer", "DJ Mixer", "DJ Mixers"),
    ]

    static func roleRank(_ role: String) -> Int { roles.firstIndex { $0.0 == role } ?? roles.count }

    static func roleName(_ role: String, count: Int) -> String {
        guard let r = roles.first(where: { $0.0 == role }) else { return role.prefix(1).uppercased() + role.dropFirst() }
        return L(count > 1 ? r.2 : r.1)
    }
}

struct InfoField {
    enum Kind {
        case text(String)
        case code(String)
        case noted(String, String)
        case links([(String, Route?)])
    }

    let label: String
    let kind: Kind
    var copy: String?

    static func text(_ label: String, _ value: String) -> InfoField { InfoField(label: label, kind: .text(value)) }
    /// IDs, ISRC and paths, in smaller text with a copy button.
    static func code(_ label: String, _ value: String) -> InfoField { InfoField(label: label, kind: .code(value), copy: value) }
    static func noted(_ label: String, _ value: String, _ note: String) -> InfoField { InfoField(label: label, kind: .noted(value, note)) }
    static func links(_ label: String, _ items: [(String, Route?)]) -> InfoField { InfoField(label: label, kind: .links(items)) }
}

/// A long paragraph in an info group, three lines until MORE opens it.
struct InfoParagraph: View {
    let text: String
    @State private var open = false

    var body: some View {
        VStack(alignment: .trailing, spacing: T.px(4)) {
            Text(text).font(.navia(T.textMD)).foregroundStyle(T.t1.color).lineSpacing(T.textMD * 0.4)
                .lineLimit(open ? nil : 3).fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
            Button(open ? L("LESS") : L("MORE")) { open.toggle() }
                .buttonStyle(.plain).font(.navia(T.textMD, .semibold)).foregroundStyle(T.t1.color)
        }
    }
}
