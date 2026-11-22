import NaviaCoreFFI
import SwiftUI

/// An album's own page, tinted from its artwork.
struct AlbumPage: View {
    let shell: ShellModel
    let id: String

    var body: some View {
        PageFrame(shell: shell, nav: .icons, edgeFade: true) {
            if let album = shell.library.album(id: id) {
                AlbumDetail(shell: shell, album: album)
            } else {
                PageScroll { EmptyStateView(symbol: "square.stack", title: L("This album isn't in your library")) }
            }
        }
    }
}

/// One album's artwork, title, actions and songs. Used as a page and inside the Albums split view.
struct AlbumDetail: View {
    let shell: ShellModel
    let album: AlbumRow
    var embedded = false
    @State private var holder = Once<any SongsViewModel>()
    @Environment(\.pageContext) private var ctx
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        let songs = holder.get { shell.library.songs(forAlbum: album.id) }
        // Decided here, where the page context is known: the table measures the header in a host of its own.
        let header = AlbumHeader(shell: shell, album: album, songs: songs, embedded: embedded, stacked: ctx.cssWidth <= 620, searchWidth: pillSearchWidth(ctx))
            .padding(.leading, ctx.sidebarUnder + ctx.gutter)
            .padding(.trailing, ctx.end + ctx.gutter + ctx.dockUnder)
            .padding(.top, PagePadding.top)
            .padding(.bottom, T.albumSectionGap)
        let hosted = header.background(alignment: .top) { if !embedded { PageTint(hex: album.tintHex ?? "2a2b33") } }

        ZStack(alignment: .top) {
            if isSnapshot {
                hosted.frame(maxHeight: .infinity, alignment: .top)
            } else if songs.totalCount == 0 {
                VStack(spacing: 0) {
                    hosted
                    EmptyStateView(title: L("No songs in this album"), compact: true)
                }
                .frame(maxHeight: .infinity, alignment: .top)
            } else {
                SongsTable(songs: songs, shell: shell, revision: songs.revision, editRevision: songs.editRevision,
                           nowPlayingID: shell.player.current?.id, variant: .album, pageHeader: AnyView(hosted),
                           bottomInset: PagePadding.bottom(hasBar: ctx.hasBar), discs: discs(songs),
                           trailing: shell.library.moreBy(album: album).map { .section(AnyView(moreBy($0))) },
                           heat: shell.deezerChoice == true ? AlbumHeat(ranks: shell.library.albumHeat(album: album), tint: Tint(hex: album.tintHex).fg) : nil)
            }
            if songs.count == 0 && songs.totalCount > 0 && !isSnapshot {
                EmptyStateView(symbol: "magnifyingglass", title: L("No songs match"),
                               hint: songs.query.isEmpty ? L("No songs on this album are in this filter.") : L("Nothing in this album matches “{query}”.", ["query": songs.query]),
                               compact: true, actionTitle: L("Clear")) { songs.query = ""; songs.filter = .all }
                    .padding(.top, PagePadding.top + T.heroArt + T.px(140))
            }
        }
        .onChange(of: album.id, initial: true) { shell.library.reaskMissingHeat(album: album) }
    }
}

extension AlbumDetail {
    /// "More by <album artist>" after the songs. It's hosted in the table, so it takes the page context and side
    /// room itself.
    fileprivate func moreBy(_ more: MoreBy) -> some View {
        let shared = more.albums.sharedNames
        return Shelf(title: L("More by {name}", ["name": more.artist.name]),
                     more: ShelfMore(title: L("See Discography")) { shell.openAlbums(.artist(more.artist.id, more.section), sort: .year) }) {
            ForEach(more.albums) { other in
                AlbumCard(album: other, byArtist: true, shell: shell, showEdition: shared.contains(other.name.lowercased())) { shell.go(.album(other.id)) }
            }
        }
        .padding(.top, T.sectionGap)
        .padding(.leading, ctx.sidebarUnder)
        .padding(.trailing, ctx.end + ctx.dockUnder)
        .environment(\.pageContext, ctx)
        .foregroundStyle(T.t1.color)
    }

    /// Disc names for the song list's headers, only when the album spans more than one disc.
    fileprivate func discs(_ songs: any SongsViewModel) -> [Int: String]? {
        Set(songs.entireList().compactMap(\.disc)).count > 1 ? album.discTitles : nil
    }
}

private struct AlbumHeader: View {
    let shell: ShellModel
    let album: AlbumRow
    let songs: any SongsViewModel
    let embedded: Bool
    /// The art above the title instead of beside it, for narrow pages.
    let stacked: Bool
    let searchWidth: CGFloat

    private var tint: Tint { Tint(hex: album.tintHex) }

    var body: some View {
        let library = shell.library
        let current = library.album(id: album.id) ?? album
        let tracks = songs.entireList()
        VStack(alignment: .leading, spacing: T.albumSectionGap) {
            AlbumHead(artworkURL: album.artworkURL, stacked: stacked) {
                AlbumTitle(name: album.name, explicit: album.isExplicit)
                artistLine.padding(.top, T.albumArtistGap)
                metaLine.padding(.top, T.albumMetaGap)
                if !album.blurb.isEmpty {
                    HeaderDescription(about: AboutText(title: album.name, subtitle: album.artist.isEmpty ? nil : album.artist, art: album.artworkURL.map { .artwork($0, .album) }, text: album.blurb), shell: shell)
                        .padding(.top, T.albumArtistGap)
                }
            }
            ActionRow(spacing: T.px(8)) {
                PlayActions(songs: tracks, shell: shell) {
                    IconButton(symbol: "heart", label: L("Favorite"), active: current.isFavorite, inGlass: true, pop: true) { library.toggleFavorite(album: album.id) }
                    DownloadToggle(kind: .album, id: album.id, name: album.name, hasSongs: !tracks.isEmpty, shell: shell)
                    MenuIconButton(symbol: "ellipsis", label: L("More")) {
                        [MenuEntry(title: L("Add to Queue"), enabled: !tracks.isEmpty) { tracks.forEach { shell.player.addToQueue($0) }; RowFlash.shared.flash(tracks.map(\.id)) },
                         SongMenu.addToPlaylist(tracks, name: album.name, shell: shell),
                         shell.shareEntry(album: current),
                         SongMenu.info(.album(album.id), shell: shell)]
                    }
                }
            } trailing: {
                GlassGroup {
                    MenuIconButton(symbol: "rectangle.split.2x1", label: L("Columns"), size: T.px(16)) { AlbumColumn.menu(deezer: shell.deezerChoice == true) }
                }
                GlassGroup {
                    SortButton(symbol: "line.3.horizontal.decrease", label: L("Filter songs"), options: SongFilter.allCases, title: \.label, current: songs.filter) { songs.filter = $0 }
                }
                SearchPill(placeholder: L("Search Album"), text: Binding(get: { songs.query }, set: { songs.query = $0 }), accent: shell.theme.accent, width: searchWidth)
            }
        }
        .foregroundStyle(T.t1.color)
    }

    @ViewBuilder private var artistLine: some View {
        if album.credits.filter({ $0.artistID != nil }).count > 1 {
            // Several album artists, each its own link.
            LinkedText(pieces: LinkPiece.artist(album), shell: shell)
                .font(.navia(T.textSubtitle, .semibold))
                .foregroundStyle(tint.fg.color)
        } else {
            AlbumArtistLine(name: album.artist, artistID: album.artistID, color: tint.fg, shell: shell)
        }
    }

    private var metaLine: some View {
        let hiRes = songs.entireList().contains(where: \.isHiRes)
        return HStack(spacing: 0) {
            if !album.genre.isEmpty {
                LinkedText(pieces: [LinkPiece(id: 0, text: album.genre, route: .songs(genre: album.genre))], shell: shell)
                    .fontWeight(.semibold).foregroundStyle(tint.fg.color)
                Text(" • ")
            }
            if let year = album.year { Text("\(String(year)) • ") }
            // The edition as its own meta item, like "Deluxe Edition".
            if let edition = album.edition { Text("\(edition) • ") }
            Text(songsAndLength(album.songCount, seconds: album.duration))
            if let status = shell.library.synced?.status(.album, album.id) { Text(" • " + SyncedDownloads.statusText(status)) }
            if hiRes {
                Text(" • ")
                Image(systemName: "waveform").font(.system(size: T.px(14))).padding(.trailing, T.px(5))
                Text(L("Hi-res Lossless"))
            }
        }
        .font(.navia(T.textMD, .medium))
        .foregroundStyle(tint.fgDim.color)
    }
}

struct AlbumHead<Info: View>: View {
    let artworkURL: URL?
    let stacked: Bool
    @ViewBuilder var info: Info

    var body: some View {
        (stacked ? AnyLayout(VStackLayout(alignment: .leading, spacing: T.albumHeadGap)) : AnyLayout(HStackLayout(alignment: .top, spacing: T.albumHeadGap))) {
            ArtworkView(url: artworkURL, pixels: 600, animates: true)
                .frame(width: T.heroArt, height: T.heroArt)
                .clipShape(RoundedRectangle(cornerRadius: T.rMD, style: .continuous))
                .artShadow()
            VStack(alignment: .leading, spacing: 0) { info }
                .padding(.top, T.albumInfoPadTop)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

struct AlbumTitle: View {
    let name: String
    let explicit: Bool

    var body: some View {
        HStack(alignment: .lastTextBaseline, spacing: T.textTitle * 0.27) {
            Text(name).font(.navia(T.textTitle, .bold)).foregroundStyle(T.tEm.color).tracking(-0.024 * T.textTitle).lineSpacing(T.px(2)).lineLimit(2).truncationMode(.tail).fullTextHelp(name)
            if explicit {
                ExplicitMark(size: T.textTitle).alignmentGuide(.lastTextBaseline) { $0[.bottom] - T.textTitle * 0.1 }
            }
        }
    }
}

struct AlbumArtistLine: View {
    let name: String
    let artistID: String?
    let color: RGBA
    let shell: ShellModel

    var body: some View {
        if let artistID {
            Button { shell.go(.artist(artistID)) } label: {
                HStack(spacing: T.px(5)) {
                    Text(name).lineLimit(1).truncationMode(.tail).fullTextHelp(name)
                    Image(systemName: "chevron.right").font(.system(size: T.px(16), weight: .bold))
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .font(.navia(T.textSubtitle, .semibold))
            .foregroundStyle(color.color)
        } else {
            Text(name).font(.navia(T.textSubtitle, .semibold)).foregroundStyle(color.color)
        }
    }
}

