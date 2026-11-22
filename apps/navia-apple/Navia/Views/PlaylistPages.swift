import NaviaCoreFFI
import SwiftUI

struct PlaylistsPage: View {
    let shell: ShellModel
    @State private var holder = Once<any PlaylistsViewModel>()
    @AppStorage("playlistLayout") private var layout = "grid"
    @Environment(\.pageContext) private var ctx

    var body: some View {
        let lists = holder.get { shell.library.playlistsList() }
        PageFrame(shell: shell, nav: .none, title: L("Playlists")) {
            GlassGroup {
                SortButton(symbol: "arrow.up.arrow.down", label: L("Sort"), options: PlaylistSort.allCases, title: \.label, current: lists.sort) { lists.sort = $0 }
                GlassDivider()
                SortButton(symbol: "line.3.horizontal.decrease", label: L("Filter"), options: PlaylistKind.allCases, title: \.label, current: lists.kind) { lists.kind = $0 }
            }
            GlassGroup {
                IconButton(symbol: "list.bullet", label: L("List view"), active: layout == "list", inGlass: true) { layout = "list" }
                IconButton(symbol: "square.grid.2x2", label: L("Grid view"), active: layout == "grid", inGlass: true) { layout = "grid" }
            }
            SearchPill(placeholder: L("Search Playlists"), text: Binding(get: { lists.query }, set: { lists.query = $0 }), accent: shell.theme.accent, width: pillSearchWidth(ctx))
            MenuIconButton(symbol: "plus", label: L("New||create"), width: T.controlH, height: T.controlH) {
                [MenuEntry(title: L("New Playlist")) { shell.openNewPlaylist() },
                 MenuEntry(title: L("New Smart Playlist")) { shell.go(.smartEditor(nil)) }]
            }.glassPill()
        } content: {
            PageScroll {
                // All, Mine, Shared (only where some playlist isn't the user's own) and Downloaded.
                SegmentedChoice(look: .glass, value: lists.scope, options: PlaylistScope.allCases.filter { lists.hasOthers || ($0 != .mine && $0 != .shared) }.map { .init(value: $0, label: $0.label) },
                                accent: shell.theme.accent, label: L("Show playlists")) { lists.scope = $0 }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, T.gutter)
                    // The stack adds a section gap after it, and the grid sits 24 below.
                    .padding(.bottom, T.px(24) - T.sectionGap)
                    .task(id: shell.library.synced?.revision ?? 0) {
                        lists.downloadedIDs = Set(shell.library.synced?.rows.filter { $0.kind == .playlist }.map(\.collectionID) ?? [])
                    }
                if lists.count == 0 {
                    if lists.scope == .downloaded && lists.query.isEmpty && lists.kind == .all {
                        EmptyStateView(symbol: "music.note.list", title: L("No downloaded playlists"), hint: L("Turn on Download on a playlist to keep it on this device and in step with the server."))
                    } else if lists.scope == .shared && lists.query.isEmpty && lists.kind == .all {
                        EmptyStateView(symbol: "music.note.list", title: L("Nothing shared yet"), hint: L("Public playlists other people make on this server show up here."))
                    } else if !lists.query.isEmpty || lists.kind != .all || lists.scope != .all {
                        EmptyStateView(symbol: "magnifyingglass", title: L("No playlists match"), hint: lists.query.isEmpty ? L("No playlists of this kind.") : L("Nothing matches “{query}”.", ["query": lists.query]), actionTitle: L("Clear Filters")) { lists.query = ""; lists.kind = .all; lists.scope = .all }
                    } else {
                        LibraryEmptyState(library: shell.library, symbol: "music.note.list", title: L("No playlists yet"), hint: L("Playlists from the server show up here."))
                    }
                } else if layout == "grid" {
                    LazyCardGrid(count: lists.count) { index, width in
                        if let playlist = lists.playlist(at: index) {
                            PlaylistCard(playlist: playlist, width: width, shell: shell) { shell.go(.playlist(playlist.id)) }
                                .rightClickMenu { PlaylistMenu.entries(playlist, shell: shell) }
                                .songDropTarget(.playlist(playlistRef(playlist), place: "card"), shell: shell, pad: T.dropPadCard)
                        }
                    }
                } else {
                    LazyVStack(spacing: 0) {
                        ForEach(0..<lists.count, id: \.self) { index in
                            if let playlist = lists.playlist(at: index) { row(playlist) }
                        }
                    }
                    .padding(.horizontal, T.gutter)
                }
            }
        }
    }

    private func row(_ playlist: PlaylistRow) -> some View {
        PlaylistListRow(playlist: playlist, shell: shell) { shell.go(.playlist(playlist.id)) }
            .rightClickMenu { PlaylistMenu.entries(playlist, shell: shell) }
            .songDropTarget(.playlist(playlistRef(playlist), place: "row"), shell: shell)
    }
}

private struct PlaylistListRow: View {
    let playlist: PlaylistRow
    let shell: ShellModel
    let open: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: open) {
            HStack(spacing: 0) {
                PlaylistCover(playlist: playlist, small: true)
                    .frame(width: T.thumb, height: T.thumb)
                    .frame(width: T.px(55), alignment: .leading)
                Text(playlist.name).font(.navia(T.textMD, .medium)).foregroundStyle(T.t1.color).lineLimit(1).fullTextHelp(playlist.name)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .layoutPriority(3)
                HStack(spacing: T.downloadMarkGap) {
                    DownloadMark(kind: .playlist, id: playlist.id, shell: shell)
                    Text(playlistSubtitle(playlist)).lineLimit(1)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                Text(longDuration(playlist.duration)).frame(maxWidth: .infinity, alignment: .leading)
            }
            .font(.navia(T.textMD))
            .foregroundStyle(T.t2.color)
            .padding(.leading, T.px(5))
            .frame(height: T.rowH)
            .background(hovering ? T.row.color : .clear)
            .overlay(alignment: .bottom) { Rectangle().fill(T.line.color).frame(height: 1) }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
    }
}

struct PlaylistPage: View {
    let shell: ShellModel
    let id: String
    @State private var holder = Once<any SongsViewModel>()
    @State private var saver = PlaylistEditor()
    /// This visit's Recommended, on playlists this user can edit.
    @State private var recommended = Once<RecommendedModel?>()
    @Environment(\.pageContext) private var ctx
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        if let playlist = shell.library.playlist(id: id) {
            let songs = holder.get {
                let model = shell.library.songs(forPlaylist: id)
                PlaylistSortMemory.restore(model, playlist: id)
                return model
            }
            detail(playlist, songs: songs)
                .onChange(of: songs.sort) { PlaylistSortMemory.save(songs, playlist: id) }
                .onChange(of: songs.descending) { PlaylistSortMemory.save(songs, playlist: id) }
        } else {
            PageFrame(shell: shell) {
                PageScroll { EmptyStateView(symbol: "music.note.list", title: L("This playlist isn't in your library")) }
            }
        }
    }

    private func detail(_ p: PlaylistRow, songs: any SongsViewModel) -> some View {
        let header = PlaylistHeader(shell: shell, playlist: p, songs: songs)
            .padding(.leading, ctx.sidebarUnder + T.gutter)
            .padding(.trailing, T.gutter + ctx.dockUnder)
            .padding(.top, PagePadding.top)
            .padding(.bottom, OrderNote.shows(p, songs) ? 0 : T.sectionGap)
            .background(alignment: .top) { PlaylistPageTint(playlist: p) }
        return PageFrame(shell: shell, nav: .icons, edgeFade: true) {
            ZStack(alignment: .top) {
                if isSnapshot {
                    header.frame(maxHeight: .infinity, alignment: .top)
                } else if songs.count == 0 {
                    VStack(spacing: 0) {
                        header
                        emptyState(p, songs)
                    }
                    .frame(maxHeight: .infinity, alignment: .top)
                } else {
                    let suggest = suggestions(p, songs)
                    SongsTable(songs: songs, shell: shell, revision: songs.revision, editRevision: songs.editRevision,
                               nowPlayingID: shell.player.current?.id, pageHeader: AnyView(header),
                               bottomInset: PagePadding.bottom(hasBar: ctx.hasBar),
                               sort: songs.sort, descending: songs.descending, reorder: reorder(p, songs), remove: remove(p, songs),
                               trailing: suggest.flatMap { trailing(p, $0, songs) }, nearEnd: suggest.map { model in { model.nearView(shell: shell) } })
                }
            }
        }
    }

    /// Recommended shows on playlists this user can edit, and not while searching or filtering.
    private func suggestions(_ p: PlaylistRow, _ songs: any SongsViewModel) -> RecommendedModel? {
        guard p.canReorder, songs.query.isEmpty, songs.filter == .all else { return nil }
        let (library, id) = (shell.library, p.id)
        return recommended.get { library.recommender(forPlaylist: id).map(RecommendedModel.init(next:)) }
    }

    /// The section's heading and rows, without songs already in the playlist. Nil until there are some.
    private func trailing(_ p: PlaylistRow, _ model: RecommendedModel, _ songs: any SongsViewModel) -> TrailingSongs? {
        let held = Set(songs.entireList().map(\.id))
        let shown = model.songs.filter { !held.contains($0.id) && !model.pending.contains($0.id) }
        guard !shown.isEmpty, model.phase == .shown || model.phase == .refreshing else { return nil }
        let (shell, source, target) = (shell, PlaySource(name: L("Recommended for {name}", ["name": p.name]), link: .playlist(p.id)), playlistRef(p))
        let head = RecommendedHead(busy: model.phase == .refreshing) { model.refresh(shell: shell) }
            .padding(.leading, ctx.sidebarUnder + T.gutter)
            .padding(.trailing, T.gutter + ctx.dockUnder)
        return TrailingSongs(heading: AnyView(head), songs: shown, revision: model.revision, dimmed: model.phase == .refreshing,
                             play: { shell.play(shown, at: $0, from: source) },
                             add: { song in
                                 model.adding(song)
                                 shell.addSongs([song], toPlaylist: target) { model.added(song, saved: $0) }
                             })
    }

    /// Songs move only on playlists this user can edit, in the playlist's own order, unfiltered.
    private func reorder(_ p: PlaylistRow, _ songs: any SongsViewModel) -> (([Int], Int) -> Void)? {
        #if DEBUG
        PlaylistOrderDebug.songs = songs as? LocalSongsModel
        #endif
        guard p.canReorder, let local = songs as? LocalSongsModel, local.showsListOrder, songs.count > 1 else { return nil }
        let (shell, saver, id) = (shell, saver, p.id)
        return { rows, before in saver.move(rows, before: before, in: local, playlist: id, shell: shell) }
    }

    /// Rows come out of playlists this user can edit, by their place in the playlist's own order.
    private func remove(_ p: PlaylistRow, _ songs: any SongsViewModel) -> (([Int]) -> Void)? {
        guard p.canReorder, let local = songs as? LocalSongsModel else { return nil }
        let (shell, saver, id, name) = (shell, saver, p.id, p.name)
        return { rows in saver.remove(rows.compactMap { local.listPlace(of: $0) }, in: local, playlist: id, name: name, shell: shell) }
    }

    @ViewBuilder private func emptyState(_ p: PlaylistRow, _ songs: any SongsViewModel) -> some View {
        if songs.totalCount == 0 {
            EmptyStateView(symbol: "music.note.list", title: L("This playlist is empty"), hint: p.isSmart ? L("No songs match its rules right now.") : L("Add songs from any song menu."), compact: true)
        } else {
            EmptyStateView(symbol: "magnifyingglass", title: L("No songs match"), hint: songs.query.isEmpty ? L("No songs in this playlist are in this filter.") : L("Nothing in this playlist matches “{query}”.", ["query": songs.query]), compact: true, actionTitle: L("Clear")) { songs.query = ""; songs.filter = .all }
        }
    }
}

/// A playlist-like page's head (a playlist, Liked Songs).
struct CoverHead<Cover: View, Meta: View>: View {
    let title: String
    @ViewBuilder let cover: Cover
    @ViewBuilder let meta: Meta

    var body: some View {
        HStack(alignment: .center, spacing: T.px(25)) {
            cover
                .frame(width: T.heroArt, height: T.heroArt)
                .artShadow()
            VStack(alignment: .leading, spacing: T.px(14)) {
                Text(title).font(.navia(T.textTitle, .bold)).foregroundStyle(T.tEm.color).tracking(-0.024 * T.textTitle).lineLimit(2).truncationMode(.tail).fullTextHelp(title)
                meta
            }
        }
    }
}

private struct PlaylistHeader: View {
    let shell: ShellModel
    let playlist: PlaylistRow
    let songs: any SongsViewModel
    @Environment(\.pageContext) private var ctx

    var body: some View {
        let tracks = songs.entireList()
        VStack(alignment: .leading, spacing: T.sectionGap + T.px(6)) {
            CoverHead(title: playlist.name) {
                PlaylistCover(playlist: playlist, animates: true)
            } meta: {
                FlowChips(spacing: T.px(12), lineSpacing: T.px(6)) {
                    Text("\(songsAndLength(playlist.songCount, seconds: playlist.duration)) • \(playlist.isPublic ? L("Public") : L("Private"))\(playlist.isMine || playlist.ownerName.isEmpty ? "" : " • " + L("By {name}", ["name": playlist.ownerName]))\(shell.library.synced?.status(.playlist, playlist.id).map { " • " + SyncedDownloads.statusText($0) } ?? "")")
                        .font(.navia(T.textMD))
                    if playlist.isSmart { SmartRulesPill(shell: shell, playlist: playlist) }
                }
                if !playlist.summary.isEmpty {
                    HeaderDescription(about: AboutText(title: playlist.name, subtitle: playlist.isMine || playlist.ownerName.isEmpty ? nil : L("By {name}", ["name": playlist.ownerName]), art: .cover(PlaylistCover(playlist: playlist)), text: playlist.summary), shell: shell)
                }
            }
            VStack(alignment: .leading, spacing: T.orderNoteGap) {
                HStack(spacing: T.px(8)) {
                    PlayActions(songs: tracks, shell: shell) {
                        DownloadToggle(kind: .playlist, id: playlist.id, name: playlist.name, hasSongs: !tracks.isEmpty, shell: shell)
                        // Share, and for the owner whether the playlist is public on the server.
                        MenuIconButton(symbol: "person.badge.plus", label: L("Share")) {
                            (playlist.isOwned ? [MenuEntry(title: L("Public on this server"), checked: shell.isPublic(playlist)) { shell.togglePublic(playlist) }] : []) + [shell.shareEntry(playlist: playlist)]
                        }
                        if playlist.isOwned {
                            MenuIconButton(symbol: "pencil", label: L("Edit")) {
                                var items = [PlaylistMenu.editDetails(playlist, shell: shell)]
                                if playlist.isSmart {
                                    items.append(MenuEntry(title: L("Edit Rules")) { shell.go(.smartEditor(playlist.id)) })
                                }
                                items.append(.divider)
                                if shell.library.canChangePlaylistImages {
                                    items.append(MenuEntry(title: L("Upload Image")) { pickPlaylistImage(playlist: playlist.id, shell: shell) })
                                    items.append(MenuEntry(title: L("Remove Image")) { removePlaylistImage(playlist: playlist.id, shell: shell) })
                                }
                                items += [.divider, PlaylistMenu.delete(playlist, shell: shell)]
                                return items
                            }
                        }
                        MenuIconButton(symbol: "ellipsis", label: L("More")) { [SongMenu.addToPlaylist(tracks, name: playlist.name, shell: shell), SongMenu.info(.playlist(playlist.id), shell: shell)] + [shell.pinEntry(playlist.id)].compactMap { $0 } }
                    }
                    Spacer(minLength: 0)
                    GlassGroup {
                        SortButton(symbol: "arrow.up.arrow.down", label: L("Sort"), options: SongSort.playlistSorts, title: \.label, current: songs.sort) { songs.sort = $0 }
                        GlassDivider()
                        SortButton(symbol: "line.3.horizontal.decrease", label: L("Filter"), options: SongFilter.allCases, title: \.label, current: songs.filter) { songs.filter = $0 }
                    }
                    SearchPill(placeholder: L("Search Playlist"), text: Binding(get: { songs.query }, set: { songs.query = $0 }), accent: shell.theme.accent, width: pillSearchWidth(ctx))
                }
                // The table follows the note directly, so the note sits evenly between the pills and the column labels.
                if OrderNote.shows(playlist, songs), let local = songs as? LocalSongsModel {
                    OrderNote(songs: local)
                }
            }
        }
    }
}

/// remembered per playlist for this account.
@MainActor enum PlaylistSortMemory {
    private static var key: String { AccountPrefs.key("playlistSorts") }

    static func restore(_ songs: any SongsViewModel, playlist id: String) {
        guard let saved = (UserDefaults.standard.dictionary(forKey: key) as? [String: String])?[id] else { return }
        let parts = saved.split(separator: ":")
        guard let sort = parts.first.flatMap({ SongSort(rawValue: String($0)) }), sort == songs.ownSort || songs.canSort(sort) else { return }
        songs.sort = sort
        songs.descending = parts.count > 1 && parts[1] == "desc"
    }

    static func save(_ songs: any SongsViewModel, playlist id: String) {
        var all = (UserDefaults.standard.dictionary(forKey: key) as? [String: String]) ?? [:]
        if songs.sort == songs.ownSort { all[id] = nil } else { all[id] = songs.sort.rawValue + (songs.descending ? ":desc" : "") }
        UserDefaults.standard.set(all, forKey: key)
    }
}

/// on a reorderable playlist that's sorted or filtered: why songs can't be moved, and the way back.
private struct OrderNote: View {
    let songs: LocalSongsModel

    static func shows(_ playlist: PlaylistRow, _ songs: any SongsViewModel) -> Bool {
        guard playlist.canReorder, let local = songs as? LocalSongsModel else { return false }
        return local.totalCount > 1 && !local.showsListOrder
    }

    var body: some View {
        let sorted = songs.sort != .order
        HStack(spacing: T.px(8)) {
            Image(systemName: "arrow.up.arrow.down").font(.system(size: T.px(13)))
            Text(sorted ? L("Sorted by {name}. Songs can only be moved in Playlist Order.", ["name": songs.sort.label])
                        : L("Showing some songs. Clear the search and filter to move songs."))
            Button {
                songs.sort = .order
                songs.filter = .all
                songs.query = ""
            } label: {
                Text(sorted ? L("Use Playlist Order") : L("Show All"))
                    .font(.navia(T.textSM, .medium))
                    .foregroundStyle(T.t1.color)
                    .padding(.horizontal, T.px(10))
                    .padding(.vertical, T.px(3))
                    .contentShape(Capsule())
            }
            .buttonStyle(HoverFillStyle(key: "UsePlaylistOrder"))
            .padding(.leading, T.px(4))
        }
        .font(.navia(T.textSM))
        .foregroundStyle(T.t2.color)
    }
}

/// Changes a playlist's songs from its page, one change at a time so each checks against the last. A new order
/// shows at once. If a save fails, the stored order comes back and the page says why. Removed rows go once the
/// server confirms.
@MainActor final class PlaylistEditor {
    private var last: Task<Void, Never>?

    func remove(_ places: [Int], in songs: LocalSongsModel, playlist id: String, name: String, shell: ShellModel) {
        guard !places.isEmpty else { return }
        let previous = last
        last = Task { @MainActor in
            await previous?.value
            let shown = songs.entireList()
            let result = await shell.library.removeFromPlaylist(id: id, expected: shown.map(\.id), places: places)
            #if DEBUG
            PlaylistOrderDebug.results.append("remove \(result)")
            #endif
            // The stored copy is the server's either way.
            if let stored = await shell.library.storedPlaylistSongs(id: id) { songs.setRows(stored) }
            switch result {
            case .saved: shell.showToast(L("Removed from {name}", ["name": name]))
            case .changed(let message), .failed(let message): shell.showToast(message)
            }
        }
    }

    func move(_ rows: [Int], before: Int, in songs: LocalSongsModel, playlist id: String, shell: ShellModel) {
        let old = songs.entireList()
        songs.move(rows, before: before)
        let new = songs.entireList()
        guard new.map(\.id) != old.map(\.id) else { return }
        let previous = last
        last = Task { @MainActor in
            await previous?.value
            let result = await shell.library.savePlaylistOrder(id: id, expected: old.map(\.id), order: new.map(\.id))
            #if DEBUG
            PlaylistOrderDebug.results.append("\(result)")
            #endif
            guard result != .saved else { return }
            let stored = await shell.library.storedPlaylistSongs(id: id)
            songs.setRows(stored ?? old)
            switch result {
            case .changed(let message), .failed(let message): shell.showToast(message)
            case .saved: break
            }
        }
    }
}

#if DEBUG
@MainActor enum PlaylistOrderDebug {
    static var results: [String] = []
    /// for CoreCheck's `playlistSort` and `playlistQuery`.
    static weak var songs: LocalSongsModel?
}
#endif

/// One visit's Recommended under a playlist: batches from the core's ranking, loaded when the end of the list is
/// near, then on Refresh.
@Observable final class RecommendedModel {
    enum Phase { case waiting, asking, shown, refreshing, empty }
    private let next: () async -> [SongRow]
    private(set) var songs: [SongRow] = []
    private(set) var phase = Phase.waiting
    private(set) var revision = 0

    init(next: @escaping () async -> [SongRow]) { self.next = next }

    /// The end of the list is near: loads the first batch, or the next once every suggestion was added.
    func nearView(shell: ShellModel) {
        if phase == .waiting { fetch(shell: shell) }
    }

    func refresh(shell: ShellModel) {
        if phase == .shown { fetch(shell: shell) }
    }

    /// Added and on their way to the server. Hidden, and shown again if the server refuses.
    private(set) var pending: Set<String> = []

    func adding(_ song: SongRow) {
        pending.insert(song.id)
        revision += 1
    }

    /// A saved suggestion leaves the list, a refused one comes back.
    func added(_ song: SongRow, saved: Bool) {
        pending.remove(song.id)
        if saved { songs.removeAll { $0.id == song.id } }
        revision += 1
        if songs.isEmpty { phase = .waiting }
    }

    private func fetch(shell: ShellModel) {
        let refreshing = !songs.isEmpty
        phase = refreshing ? .refreshing : .asking
        Task { @MainActor in
            let batch = await next()
            if !batch.isEmpty {
                songs = batch
                revision += 1
                phase = .shown
            } else if refreshing {
                phase = .shown
                shell.showToast(L("No other suggestions right now"))
            } else {
                phase = .empty
            }
        }
    }
}

/// "Recommended" with its hint, and Refresh at the right, level with the hint.
private struct RecommendedHead: View {
    let busy: Bool
    let refresh: () -> Void

    var body: some View {
        HStack(alignment: .bottom, spacing: T.shelfHeadGap) {
            VStack(alignment: .leading, spacing: T.px(2)) {
                SectionTitle(title: L("Recommended")).foregroundStyle(T.t1.color)
                Text(L("Based on the songs in this playlist")).font(.navia(T.textSM)).foregroundStyle(T.t2.color)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            ActionButton(title: busy ? L("Refreshing…") : L("Refresh"), small: true, enabled: !busy, action: refresh)
        }
        .padding(.top, T.sectionGap)
        .padding(.bottom, T.shelfHeadGap)
    }
}
