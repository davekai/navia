import NaviaCoreFFI
import SwiftUI

/// What's saved on this device: total size, kept playlists and albums, then songs downloaded on their own.
struct DownloadsPage: View {
    let shell: ShellModel
    @State private var entries: [DownloadEntry]?
    @State private var singles: Set<String> = []
    @Environment(\.pageContext) private var ctx

    var body: some View {
        PageFrame(shell: shell, nav: .none, title: L("Downloads")) {
            PageScroll { content }
        }
        .task(id: "\(shell.library.collectionRevision)-\(shell.library.synced?.revision ?? 0)") {
            entries = await shell.library.downloadEntries()
            singles = shell.library.synced?.singleIDs() ?? Set(entries?.map(\.song.id) ?? [])
        }
    }

    @ViewBuilder private var content: some View {
        if let entries {
            if entries.isEmpty && (shell.library.synced?.rows.isEmpty ?? true) {
                EmptyStateView(title: L("No downloads yet"), hint: L("Songs, albums and playlists you download are saved on this device and play without the server."))
            } else {
                list(entries)
            }
        }
    }

    private func list(_ entries: [DownloadEntry]) -> some View {
        let total = entries.reduce(Int64(0)) { $0 + $1.bytes }
        let columns = ListColumns(ctx, buttons: 1)
        let compact = columns.compact
        return VStack(alignment: .leading, spacing: T.px(16)) {
            HStack(spacing: T.px(16)) {
                (Text(countLabel(entries.count)).foregroundStyle(T.t1.color).fontWeight(.medium)
                    + Text(" · " + L("{size} on this device", ["size": formatSize(total)])).foregroundStyle(T.t2.color))
                    .font(.navia(T.textMD))
                Spacer(minLength: 0)
                ActionButton(title: L("Clear All"), symbol: "trash") { confirmClear(entries, total: total) }
            }
            let kept = shell.library.synced?.rows ?? []
            let own = entries.filter { singles.contains($0.song.id) }
            if !kept.isEmpty {
                VStack(alignment: .leading, spacing: T.px(6)) {
                    Text(L("Playlists and Albums")).font(.navia(T.textSection, .semibold)).foregroundStyle(T.t1.color)
                    Text(L("Kept in step with the server: songs added download by themselves, and songs taken out are removed unless another download holds them."))
                        .font(.navia(T.textSM)).foregroundStyle(T.t2.color).fixedSize(horizontal: false, vertical: true).padding(.bottom, T.px(4))
                    VStack(spacing: 0) {
                        ForEach(kept) { row in
                            CollectionRow(shell: shell, row: row, compact: compact, nameWidth: columns.name, stateWidth: columns.side)
                        }
                    }
                }
                .padding(.top, T.px(14))
            }
            if !own.isEmpty {
                VStack(alignment: .leading, spacing: T.px(6)) {
                    if !kept.isEmpty { Text(L("Songs")).font(.navia(T.textSection, .semibold)).foregroundStyle(T.t1.color).padding(.top, T.px(14)) }
                    LazyVStack(spacing: 0) {
                        ForEach(Array(own.enumerated()), id: \.element.song.id) { index, entry in
                            DownloadRow(shell: shell, entry: entry, compact: compact, nameWidth: columns.name, albumWidth: columns.side) {
                                shell.player.play(own.map(\.song), at: index, shuffle: false)
                            }
                        }
                    }
                }
            }
        }
        .padding(.horizontal, T.gutter)
        .frame(maxWidth: T.px(980))
        .frame(maxWidth: .infinity)
    }

    private func confirmClear(_ entries: [DownloadEntry], total: Int64) {
        let songs = entries.map(\.song)
        let kept = shell.library.synced?.rows.count ?? 0
        var message = L("{songs} and {size} will be removed from this device. Nothing changes on the server.", ["songs": countLabel(entries.count), "size": formatSize(total)])
        if kept > 0 { message += " " + Ln("{count} playlist or album stops downloading.", "{count} playlists and albums stop downloading.", kept) }
        shell.confirm(ConfirmRequest(title: L("Clear all downloads?"), message: message, confirmTitle: L("Clear All")) { [library = shell.library] in
            // Kept playlists and albums stop too, so nothing downloads again.
            if let synced = library.synced { synced.clearAll() } else { library.setDownloaded(songs: songs, false) }
        })
    }

    private func countLabel(_ n: Int) -> String { Ln("{count} song", "{count} songs", n) }
}

private struct DownloadRow: View {
    let shell: ShellModel
    let entry: DownloadEntry
    let compact: Bool
    let nameWidth: CGFloat
    let albumWidth: CGFloat
    let onPlay: () -> Void
    @State private var hovering = false

    var body: some View {
        HStack(spacing: T.px(14)) {
            ListSongCells(song: entry.song, bytes: entry.bytes, compact: compact, nameWidth: nameWidth, albumWidth: albumWidth, playing: entry.song.id == shell.player.current?.id ? shell.theme.accentSoft.color : nil, onName: onPlay)
            ListRowButton(symbol: "xmark", help: L("Remove Download"), label: L("Remove download of {title}", ["title": entry.song.title])) { shell.library.setDownloaded(songs: [entry.song], false) }
        }
        .padding(.horizontal, T.listRowPadX)
        .frame(height: T.listRowH)
        .background(RoundedRectangle(cornerRadius: T.rRow, style: .continuous).fill(hovering ? T.row.color : .clear))
        .onHover { hovering = $0 }
    }
}

/// A kept playlist or album: its cover, kind, size, progress and switch. The X asks first.
private struct CollectionRow: View {
    let shell: ShellModel
    let row: SyncedDownloads.Row
    let compact: Bool
    let nameWidth: CGFloat
    let stateWidth: CGFloat
    @State private var hovering = false

    var body: some View {
        let playlist = row.kind == .playlist ? shell.library.playlist(id: row.collectionID) : nil
        let album = row.kind == .album ? shell.library.album(id: row.collectionID) : nil
        let name = playlist?.name ?? album?.name ?? ""
        let kind = playlist.map { $0.isSmart ? L("Smart Playlist") : L("Playlist") } ?? L("Album")
        let count = playlist?.songCount ?? album?.songCount ?? 0
        let sub = ([kind] + (album.map { [$0.artist] } ?? []) + [Ln("{count} song", "{count} songs", count)]).joined(separator: " • ")
        let route: Route = row.kind == .playlist ? .playlist(row.collectionID) : .album(row.collectionID)
        HStack(spacing: T.px(14)) {
            Group {
                if let playlist { PlaylistCover(playlist: playlist, small: true) } else { ArtworkView(url: album?.artworkURL, pixels: 120) }
            }
            .frame(width: T.px(42), height: T.px(42))
            .clipShape(RoundedRectangle(cornerRadius: T.rSM, style: .continuous))
            Button { shell.go(route) } label: {
                VStack(alignment: .leading, spacing: T.px(1)) {
                    Text(name).font(.navia(T.textMD, .medium)).foregroundStyle(T.t1.color).lineLimit(1).fullTextHelp(name)
                    Text(sub).font(.navia(T.textSM)).foregroundStyle(T.t2.color).lineLimit(1).fullTextHelp(sub)
                }
                .frame(maxWidth: compact ? .infinity : nameWidth, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if !compact {
                HStack(spacing: T.downloadMarkGap) {
                    if row.status.phase != .done {
                        DownloadGlyph(kind: SyncedDownloads.glyph(row.status), progress: row.status.progress, size: T.downloadMark, theme: shell.theme, ink: T.t2)
                    }
                    Text(SyncedDownloads.statusText(row.status)).lineLimit(1)
                }
                .font(.navia(T.textSM)).foregroundStyle(T.t2.color)
                .frame(width: stateWidth, alignment: .leading)
            }
            Text(formatSize(Int64(row.status.bytes))).font(.navia(T.textSM).monospacedDigit()).foregroundStyle(T.t2.color)
                .frame(width: T.px(72), alignment: .trailing)
            ListRowButton(symbol: "xmark", help: L("Remove Download"), label: L("Remove download of {title}", ["title": name])) { shell.toggleCollectionDownload(row.kind, id: row.collectionID, name: name) }
        }
        .padding(.horizontal, T.listRowPadX)
        .frame(height: T.listRowH)
        .background(RoundedRectangle(cornerRadius: T.rRow, style: .continuous).fill(hovering ? T.row.color : .clear))
        .onHover { hovering = $0 }
    }
}

/// A list page row's column widths: art, then name and album sharing the rest 2 to 1.3, then size and buttons.
/// `compact` drops the album under 620 wide.
struct ListColumns {
    let compact: Bool
    let name: CGFloat
    let side: CGFloat

    init(_ ctx: PageContext, buttons: Int) {
        compact = ctx.cssWidth <= 620
        let rowWidth = min(T.listPageW - 2 * T.gutter, (ctx.layoutWidth > 0 ? ctx.layoutWidth : ctx.pageWidth) - 2 * T.gutter) - 2 * T.listRowPadX
        let shared = max(0, rowWidth - T.thumb - T.px(72) - CGFloat(buttons) * T.px(38) - CGFloat(3 + buttons) * T.px(14))
        name = shared * 2 / 3.3
        side = shared * 1.3 / 3.3
    }
}

/// A list page's song cells: art, title over artist, the album when there's room, and the size. `onName` plays the
/// song where the list has no selection. Without it, clicks go to the row.
struct ListSongCells: View {
    let song: SongRow
    let bytes: Int64
    let compact: Bool
    let nameWidth: CGFloat
    let albumWidth: CGFloat
    /// The title's color while it's the song playing.
    var playing: Color? = nil
    var onName: (() -> Void)? = nil
    /// off where the row draws the art itself.
    var art = true

    var body: some View {
        if art { ListSongArt(song: song).allowsHitTesting(onName != nil) }
        if let onName {
            Button(action: onName) { name }
                .buttonStyle(.plain)
                .help(L("Play {title}", ["title": song.title]))
        } else {
            name.allowsHitTesting(false)
        }
        if !compact {
            Text(song.album).font(.navia(T.textSM)).foregroundStyle(T.t2.color).lineLimit(1).fullTextHelp(song.album)
                .frame(width: albumWidth, alignment: .leading)
                .allowsHitTesting(false)
        }
        Text(formatSize(bytes)).font(.navia(T.textSM).monospacedDigit()).foregroundStyle(T.t2.color)
            .frame(width: T.px(72), alignment: .trailing)
            .allowsHitTesting(false)
    }

    private var name: some View {
        VStack(alignment: .leading, spacing: T.px(1)) {
            Text(song.title).font(.navia(T.textMD, .medium)).foregroundStyle(playing ?? T.t1.color).lineLimit(1).fullTextHelp(song.title)
            Text(song.artist).font(.navia(T.textSM)).foregroundStyle(T.t2.color).lineLimit(1).fullTextHelp(song.artist)
        }
        .frame(maxWidth: compact ? .infinity : nameWidth, alignment: .leading)
        .contentShape(Rectangle())
    }
}

/// A list page row's artwork (`T.thumb`, `T.rSM`).
struct ListSongArt: View {
    let song: SongRow

    var body: some View {
        ArtworkView(url: song.artworkURL, pixels: 120)
            .frame(width: T.thumb, height: T.thumb)
            .clipShape(RoundedRectangle(cornerRadius: T.rSM, style: .continuous))
    }
}

/// A list page row's button at its end. The icon brightens and a pill fills on hover.
struct ListRowButton: View {
    let symbol: String
    let help: String
    let label: String
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: T.px(13), weight: .medium))
                .foregroundStyle((hovering ? T.t1 : T.t2).color)
                .frame(width: T.px(38), height: T.px(34))
                .background(Capsule().fill(hovering ? T.hover.color : .clear))
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .help(help)
        .accessibilityLabel(label)
    }
}

func formatSize(_ bytes: Int64) -> String {
    let value = Double(bytes)
    if value >= 1e9 { return "\(fmtDec(value / 1e9, 1)) GB" }
    if value >= 1e6 { return "\(fmtInt(Int((value / 1e6).rounded()))) MB" }
    return "\(fmtInt(max(1, Int((value / 1e3).rounded())))) KB"
}
