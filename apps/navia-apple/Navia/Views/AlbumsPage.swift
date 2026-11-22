import SwiftUI

struct AlbumsPage: View {
    let shell: ShellModel
    let scope: AlbumScope
    @State private var holder = Once<any AlbumsViewModel>()
    @State private var selectedAll = false
    @State private var selected: String?
    @AppStorage("albumLayout") private var layout = PageLayout.grid.rawValue
    @Environment(\.pageContext) private var ctx
    @Environment(\.isSnapshot) private var isSnapshot

    private var heading: String {
        if case .artist(_, let section) = scope { return section.title }
        return L("Albums")
    }

    private var ownerName: String? {
        if case .artist(let id, _) = scope { return shell.library.artist(id: id)?.name }
        return nil
    }

    /// Cards name the album's artist, except on an artist's own sections.
    private var byArtist: Bool {
        if case .artist(_, let section) = scope { return section != .appearsOn }
        return false
    }

    var body: some View {
        let albums = holder.get {
            let model = shell.library.albums(scope: scope)
            // A link can ask for a sort, such as Home's Recently Added opening Albums by date added.
            if let sort = shell.takeAlbumsSort() { model.sort = sort }
            return model
        }
        let genres = shell.library.genres
        PageFrame(shell: shell, nav: .none, title: heading, detail: ownerName, edgeFade: layout == PageLayout.split.rawValue && albums.count > 0, refresh: .library) {
            GlassGroup {
                SortButton(symbol: "arrow.up.arrow.down", label: L("Sort"), options: sorts, title: \.label, current: albums.sort) { albums.sort = $0 }
                GlassDivider()
                MenuIconButton(symbol: "line.3.horizontal.decrease", label: L("Filter"), size: T.px(16)) {
                    [MenuEntry(title: L("All Genres"), checked: albums.genre == nil) { albums.genre = nil }]
                        + genres.prefix(10).map { g in MenuEntry(title: g, checked: albums.genre == g) { albums.genre = g } }
                }
            }
            LayoutToggle(layout: $layout)
            SearchPill(placeholder: L("Search Albums"), text: Binding(get: { albums.query }, set: { albums.query = $0 }), accent: shell.theme.accent, width: pillSearchWidth(ctx))
        } content: {
            if albums.count == 0 {
                PageScroll { emptyState(albums) }
            } else if layout == PageLayout.split.rawValue {
                split(albums)
            } else {
                grid(albums)
            }
        }
    }

    private var sorts: [AlbumSort] { [.plays, .recent, .name, .year] }

    @ViewBuilder private func emptyState(_ albums: any AlbumsViewModel) -> some View {
        if !albums.query.isEmpty || albums.genre != nil {
            EmptyStateView(symbol: "magnifyingglass", title: L("No albums match"),
                           hint: albums.query.isEmpty ? L("No albums here are in this genre.") : L("Nothing here matches “{query}”.", ["query": albums.query]),
                           actionTitle: L("Clear Filters")) { albums.query = ""; albums.genre = nil }
        } else {
            LibraryEmptyState(library: shell.library, symbol: "square.stack", title: L("No albums"), hint: L("Albums on the server show up here."))
        }
    }

    private func grid(_ albums: any AlbumsViewModel) -> some View {
        PageScroll {
            LazyCardGrid(count: albums.count) { index, width in
                if let album = albums.album(at: index) {
                    AlbumCard(album: album, width: width, byArtist: byArtist, shell: shell) { shell.go(.album(album.id)) }
                }
            }
        }
    }

    private func split(_ albums: any AlbumsViewModel) -> some View {
        let current: AlbumRow? = selectedAll ? nil : (selected.flatMap { id in (0..<albums.count).lazy.compactMap { albums.album(at: $0) }.first { $0.id == id } } ?? albums.album(at: 0))
        return SplitView {
            SplitItem(on: current == nil, title: L("All Albums"), subtitle: "\(albumCount(albums.count)) • \(songCount(albums.totalSongs))", action: { selectedAll = true; selected = nil }) {
                Image(systemName: "square.stack").font(.system(size: T.splitGlyph))
            }
            ForEach(0..<albums.count, id: \.self) { index in
                if let album = albums.album(at: index) {
                    SplitItem(on: album.id == current?.id, title: album.name, subtitle: [album.year.map(String.init), songCount(album.songCount)].compactMap { $0 }.joined(separator: " • "),
                              action: { selectedAll = false; selected = album.id }) {
                        ArtworkView(url: album.artworkURL, pixels: Int(T.splitArt * 2))
                            .clipShape(RoundedRectangle(cornerRadius: T.rSM, style: .continuous))
                    }
                }
            }
        } detail: {
            if let current {
                AlbumDetail(shell: shell, album: current, embedded: true).id(current.id)
            } else {
                allSongs(albums)
            }
        }
    }

    private func allSongs(_ albums: any AlbumsViewModel) -> some View {
        let songs = albums.listedAlbumsSongs()
        return SongsTable(songs: songs, shell: shell, revision: songs.revision, editRevision: songs.editRevision, nowPlayingID: shell.player.current?.id,
                          bottomInset: PagePadding.bottom(hasBar: ctx.hasBar))
    }
}
