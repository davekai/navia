import SwiftUI

/// Settings > Storage > Cached Songs. A song can be played, kept offline, or removed from the cache. Keeping it
/// offline makes it a download, reusing the cached file when the quality matches.
struct CachedSongsPage: View {
    let shell: ShellModel
    @State private var entries: [CachedSong]?
    @State private var sort: Sort = .recent
    @State private var selection = RowSelection()
    @Environment(\.pageContext) private var ctx

    enum Sort: String { case recent, size }

    var body: some View {
        PageFrame(shell: shell, nav: .none, title: L("Cached Songs")) {
            PageScroll { content }
        }
        // A song kept offline leaves the cache, so reload when downloads change.
        .task(id: shell.library.collectionRevision) { await reload() }
    }

    private func reload() async {
        guard let storage = shell.library.storage else { entries = []; return }
        entries = await storage.cachedSongs()
        selection.clear()
    }

    private var sorted: [CachedSong] {
        let rows = entries ?? []
        switch sort {
        case .recent: return rows.sorted { $0.playedMs > $1.playedMs }
        case .size: return rows.sorted { ($0.bytes, $0.playedMs) > ($1.bytes, $1.playedMs) }
        }
    }

    @ViewBuilder private var content: some View {
        if let entries {
            if entries.isEmpty {
                let off = shell.library.storage?.limit == 0
                EmptyStateView(symbol: "music.note", title: L("No cached songs"),
                               hint: off ? L("Nothing is kept while the song cache is off.") : L("Songs you play are kept here as they stream, so they start at once and play offline."))
            } else {
                list(sorted)
            }
        }
    }

    private func list(_ rows: [CachedSong]) -> some View {
        let columns = ListColumns(ctx, buttons: 2)
        let chosen = selection.selected.sorted().filter { $0 < rows.count }.map { rows[$0] }
        return VStack(alignment: .leading, spacing: T.listPageGap) {
            summary(rows, chosen: chosen)
            LazyVStack(spacing: 0) {
                ForEach(Array(rows.enumerated()), id: \.element.song.id) { index, entry in
                    row(entry, index: index, rows: rows, columns: columns)
                }
            }
        }
        .padding(.horizontal, T.gutter)
        .frame(maxWidth: T.listPageW)
        .frame(maxWidth: .infinity)
    }

    private func summary(_ rows: [CachedSong], chosen: [CachedSong]) -> some View {
        let total = rows.reduce(Int64(0)) { $0 + $1.bytes }
        let limit = shell.library.storage?.limit ?? 0
        let used = limit > 0
            ? L("{used} of {limit}", ["used": formatSize(total), "limit": StorageSettings.limitLabel(limit)])
            : L("{size} on this device", ["size": formatSize(total)])
        return HStack(spacing: T.px(16)) {
            if chosen.isEmpty {
                (Text(Ln("{count} song", "{count} songs", rows.count)).foregroundStyle(T.t1.color).fontWeight(.medium)
                    + Text(" · " + used).foregroundStyle(T.t2.color))
                    .font(.navia(T.textMD)).lineLimit(1)
            } else {
                Text(Ln("{count} song selected", "{count} songs selected", chosen.count)).font(.navia(T.textMD, .medium)).foregroundStyle(T.t1.color)
            }
            Spacer(minLength: 0)
            HStack(spacing: T.buttonGap) {
                GlassSelect(shell: shell, label: L("Sort"), value: sort.rawValue,
                            options: [SelectOption(value: Sort.recent.rawValue, label: L("Recently Played")), SelectOption(value: Sort.size.rawValue, label: L("Size"))]) { value in
                    sort = Sort(rawValue: value) ?? .recent
                    selection.clear()
                }
                .fixedSize()
                if chosen.isEmpty {
                    ActionButton(title: L("Clear Cache"), symbol: "trash") { confirmClear(count: rows.count, total: total) }
                } else {
                    ActionButton(title: L("Keep Offline"), symbol: "arrow.down") { keepOffline(chosen) }
                    ActionButton(title: L("Remove from Cache"), symbol: "xmark") { remove(chosen) }
                }
            }
        }
    }

    private func row(_ entry: CachedSong, index: Int, rows: [CachedSong], columns: ListColumns) -> some View {
        let playing = entry.song.id == shell.player.current?.id
        return SelectableRow(index: index, selection: selection, height: T.listRowH, radius: T.rRow, hoverFill: T.row.color,
                             selectedFill: shell.theme.rowSelected.color, leadWidth: T.thumb,
                             onPlay: { play(rows, at: $0) }, menu: { menu(rows, at: $0) }, onReturn: { play(rows, at: index) },
                             leadPad: T.listRowPadX, trailPad: T.listRowPadX, onDelete: { remove(chosenOr(index, rows)) }) {
            ListSongArt(song: entry.song).allowsHitTesting(false)
        } text: {
            ListSongCells(song: entry.song, bytes: entry.bytes, compact: columns.compact, nameWidth: columns.name, albumWidth: columns.side,
                          playing: playing ? shell.theme.accentSoft.color : nil, art: false)
        } trailing: {
            HStack(spacing: 0) {
                if !entry.inLibrary || shell.library.isDownloaded(song: entry.song) {
                    // Already downloaded, or gone from the server so it can't be.
                    Color.clear.frame(width: T.px(38), height: 1)
                } else if let progress = shell.library.downloadProgress(song: entry.song) {
                    DownloadProgressButton(fraction: progress, label: L("Cancel Download")) { shell.library.setDownloaded(songs: [entry.song], false) }
                        .frame(width: T.px(38))
                } else {
                    ListRowButton(symbol: "arrow.down", help: L("Keep Offline"), label: L("Keep Offline")) { keepOffline([entry]) }
                }
                ListRowButton(symbol: "xmark", help: L("Remove from Cache"), label: L("Remove from Cache")) { remove([entry]) }
            }
        }
    }

    private func chosenOr(_ index: Int, _ rows: [CachedSong]) -> [CachedSong] {
        let picked = selection.selected.contains(index) ? selection.selected.sorted() : [index]
        return picked.filter { $0 < rows.count }.map { rows[$0] }
    }

    private func menu(_ rows: [CachedSong], at index: Int) -> [MenuEntry] {
        let chosen = chosenOr(index, rows)
        var entries: [MenuEntry] = []
        if chosen.count == 1, chosen[0].inLibrary {
            entries.append(MenuEntry(title: L("Play"), symbol: "play.fill") { play(rows, at: index) })
        }
        if chosen.contains(where: \.inLibrary) {
            let title = chosen.count == 1 ? L("Keep Offline") : Ln("Keep {count} Song Offline", "Keep {count} Songs Offline", chosen.count)
            entries.append(MenuEntry(title: title, symbol: "arrow.down") { keepOffline(chosen) })
        }
        let title = chosen.count == 1 ? L("Remove from Cache") : Ln("Remove {count} Song from Cache", "Remove {count} Songs from Cache", chosen.count)
        entries.append(MenuEntry(title: title, symbol: "xmark") { remove(chosen) })
        return entries
    }

    /// Plays the list from this song, using the songs the library still has, in page order.
    private func play(_ rows: [CachedSong], at index: Int) {
        guard index < rows.count, rows[index].inLibrary else { return }
        let songs = rows.filter(\.inLibrary).map(\.song)
        let at = rows[..<index].filter(\.inLibrary).count
        shell.player.play(songs, at: at, shuffle: false)
    }

    /// Makes them downloads. A cached copy at the download quality moves over without the server.
    private func keepOffline(_ chosen: [CachedSong]) {
        let songs = chosen.filter { $0.inLibrary && !shell.library.isDownloaded(song: $0.song) }.map(\.song)
        guard !songs.isEmpty else { return }
        shell.library.setDownloaded(songs: songs, true)
        selection.clear()
    }

    private func remove(_ chosen: [CachedSong]) {
        guard !chosen.isEmpty, let storage = shell.library.storage else { return }
        let ids = Set(chosen.map(\.song.id))
        // Removed from the page at once. The files are deleted off the main thread.
        entries?.removeAll { ids.contains($0.song.id) }
        selection.clear()
        Task {
            await storage.removeCached(Array(ids))
            entries = await storage.cachedSongs()
        }
    }

    private func confirmClear(count: Int, total: Int64) {
        guard let storage = shell.library.storage else { return }
        let message = Ln("{count} song and {size} will be removed from this device. Downloads stay, and the song streams from the server the next time it plays.",
                         "{count} songs and {size} will be removed from this device. Downloads stay, and the songs stream from the server the next time they play.",
                         count, ["size": formatSize(total)])
        shell.confirm(ConfirmRequest(title: L("Clear cached songs?"), message: message, confirmTitle: L("Clear Cache")) {
            entries = []
            selection.clear()
            Task {
                await storage.clearSongs()
                entries = await storage.cachedSongs()
            }
        })
    }
}
