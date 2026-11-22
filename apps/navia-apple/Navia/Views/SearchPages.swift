import SwiftUI

/// Search and Browse share one page and frame, so the field keeps its identity, focus and text as results appear.
struct SearchPage: View {
    let shell: ShellModel
    @State private var sort: SearchSort = .relevance
    @State private var favoritesOnly = false
    @State private var scope = L("All Results")
    /// The last finished search. It stays until the next one lands, so typing doesn't flash.
    @State private var results: SearchResults?
    @State private var resultsFor: SearchKey?
    @Environment(\.pageContext) private var ctx
    @Environment(\.isSnapshot) private var isSnapshot

    private let scopes = [L("All Results"), L("Artists"), L("Albums"), L("Songs")]

    private var query: String {
        if case .search(let text) = shell.route { return text }
        return ""
    }

    /// everything a search depends on; a new key cancels the last search.
    private struct SearchKey: Hashable {
        let query: String
        let sort: SearchSort
        let favoritesOnly: Bool
        let revision: Int
    }

    private var key: SearchKey { SearchKey(query: query, sort: sort, favoritesOnly: favoritesOnly, revision: shell.library.collectionRevision) }

    var body: some View {
        let searching = !query.isEmpty
        PageFrame(shell: shell, nav: .search(query), hasRight: searching) {
            if searching {
                GlassGroup {
                    SortButton(symbol: "arrow.up.arrow.down", label: L("Sort"), options: SearchSort.allCases, title: \.label, current: sort) { sort = $0 }
                    GlassDivider()
                    MenuIconButton(symbol: "line.3.horizontal.decrease", label: L("Filter"), size: T.px(16)) {
                        [MenuEntry(title: L("Everything"), checked: !favoritesOnly) { favoritesOnly = false },
                         MenuEntry(title: L("Favorites"), checked: favoritesOnly) { favoritesOnly = true }]
                    }
                }
                GlassGroup {
                    MenuTextButton(title: scope) {
                        scopes.map { s in MenuEntry(title: s, checked: s == scope) { scope = s } }
                    }
                }
            }
        } content: {
            if !searching {
                browse
            } else if let shown = isSnapshot ? (shell.library as? SampleLibrary)?.search(query, sort: sort, favoritesOnly: favoritesOnly) : results {
                resultsView(shown, settled: isSnapshot || resultsFor == key)
            } else {
                Color.clear
            }
        }
        .task(id: key) { await runSearch(for: key) }
    }

    /// Runs after the view settles on a key. SwiftUI cancels it when the key changes, so an older search can't land.
    private func runSearch(for key: SearchKey) async {
        guard !key.query.isEmpty else {
            results = nil
            resultsFor = nil
            return
        }
        if results != nil {
            // Keys arrive faster than a full-text query on a big library, so only the end of a burst is worth asking for.
            try? await Task.sleep(for: .milliseconds(60))
            guard !Task.isCancelled else { return }
        }
        let found = await shell.library.searchResults(key.query, sort: key.sort, favoritesOnly: key.favoritesOnly)
        guard !Task.isCancelled, key == self.key else { return }
        results = found
        resultsFor = key
    }

    /// The genre tiles, with the user's top genres first and the rest by album count.
    private var browse: some View {
        let page = shell.library.genrePage
        return PageScroll {
            if page.all.isEmpty {
                LibraryEmptyState(library: shell.library, symbol: "guitars", title: L("No genres yet"), hint: L("Genres from the server's tags show up here."))
            } else {
                if !page.top.isEmpty {
                    Shelf(title: L("Your Genres"), tiles: true) {
                        ForEach(page.top, id: \.name) { genre in GenreFanTile(genre: genre) { shell.go(.songs(genre: genre.name)) } }
                    }
                }
                if !page.browse.isEmpty {
                    Shelf(title: L("Browse by Genre"), tiles: true, lazy: true) {
                        ForEach(page.browse, id: \.name) { genre in GenreFanTile(genre: genre) { shell.go(.songs(genre: genre.name)) } }
                    }
                }
            }
        }
    }

    private func show(_ what: String) -> Bool { scope == L("All Results") || scope == what }

    private var noResultsTitle: String {
        if favoritesOnly {
            if scope == L("Artists") { return L("No favorite artists") }
            if scope == L("Albums") { return L("No favorite albums") }
            if scope == L("Songs") { return L("No favorite songs") }
            return L("No favorite results")
        }
        if scope == L("Artists") { return L("No artists") }
        if scope == L("Albums") { return L("No albums") }
        if scope == L("Songs") { return L("No songs") }
        return L("No results")
    }

    /// `settled` is false while a newer search is running, so "No results" never shows for a query not yet asked.
    private func resultsView(_ results: SearchResults, settled: Bool) -> some View {
        let top = results.topArtist
        let topSongs = top.map { shell.library.topSongs(for: $0) } ?? results.topArtistSongs
        let showTop = top != nil && show(L("Artists"))
        let showSongs = (top == nil || scope == L("Songs")) && show(L("Songs")) && !results.songs.isEmpty
        let showArtists = show(L("Artists")) && !results.artists.isEmpty
        let upcoming = favoritesOnly ? [] : shell.library.upcomingMatching(query)
        let showAlbums = show(L("Albums")) && !(results.albums.isEmpty && upcoming.isEmpty)
        let nothing = !showTop && !showSongs && !showArtists && !showAlbums
        return PageScroll {
            if showTop, let top {
                topResult(top, songs: topSongs)
            }
            if showSongs { SongColumnsView(songs: results.songs, shell: shell, playsAsRadio: true) }
            if showArtists {
                Shelf(title: L("Artists"), action: { shell.go(.artists) }) {
                    ForEach(results.artists) { artist in ArtistCard(artist: artist) { shell.go(.artist(artist.id)) } }
                }
            }
            if showAlbums {
                Shelf(title: L("Albums"), action: { shell.go(.albums(.all)) }) {
                    let shared = results.albums.sharedNames
                    ForEach(results.albums) { album in AlbumCard(album: album, shell: shell, showEdition: shared.contains(album.name.lowercased())) { shell.go(.album(album.id)) } }
                    ForEach(upcoming) { album in UpcomingCard(album: album, tag: true) { shell.go(.upcoming(album.id)) } }
                }
            }
            if nothing && settled {
                EmptyStateView(symbol: "magnifyingglass", title: noResultsTitle,
                               hint: scope == L("All Results") && !favoritesOnly ? L("Nothing matches “{query}”. Check the spelling, or try an artist or album name.", ["query": query]) : L("Nothing here for “{query}”. Try a wider filter.", ["query": query]),
                               actionTitle: scope != L("All Results") || favoritesOnly ? L("Show All Results") : nil,
                               action: scope != L("All Results") || favoritesOnly ? { scope = L("All Results"); favoritesOnly = false } : nil)
            }
        }
    }

    private func topResult(_ top: ArtistRow, songs: [SongRow]) -> some View {
        let library = shell.library
        let radio = library.similarSongs(for: top)
        return VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: T.px(14)) {
                Button { shell.go(.artist(top.id)) } label: {
                    HStack(spacing: T.px(14)) {
                        ArtworkView(url: top.photoURL, kind: .artist, pixels: 120)
                            .frame(width: T.px(54), height: T.px(54))
                            .clipShape(Circle())
                        Text(top.name).font(.navia(T.px(24), .semibold))
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                Spacer(minLength: 0)
                GlassGroup {
                    Button { shell.player.play(songs, at: 0, shuffle: false) } label: {
                        HStack(spacing: T.px(9)) {
                            Image(systemName: "play.fill").font(.system(size: T.px(16)))
                            Text(L("Play")).font(.navia(T.textMD, .medium))
                        }
                        .padding(.leading, T.px(13)).padding(.trailing, T.px(12))
                        .frame(height: T.iconButtonH).contentShape(Capsule())
                    }
                    .buttonStyle(.plain)
                    .disabled(songs.isEmpty)
                    .opacity(songs.isEmpty ? T.disabledOpacity : 1)
                }
                GlassGroup {
                    MenuIconButton(symbol: "ellipsis", label: L("More")) {
                        [MenuEntry(title: L("Go to Artist")) { shell.go(.artist(top.id)) },
                         MenuEntry(title: L("Artist Radio"), enabled: !radio.isEmpty) { shell.startArtistRadio(top) },
                         shell.shareEntry(artist: top),
                         SongMenu.info(.artist(top.id), shell: shell)]
                    }
                }
            }
            .padding(.leading, T.px(16))
            .padding(.trailing, T.px(24))
            .padding(.bottom, T.px(12))
            if !songs.isEmpty {
                SongColumnsView(songs: Array(songs.prefix(6)), shell: shell, gutter: T.px(12), playsAsRadio: true)
            }
        }
        .padding(.top, T.px(16))
        .padding(.bottom, T.px(12))
        .background(T.panel2.color, in: RoundedRectangle(cornerRadius: T.rLG, style: .continuous))
        .padding(.horizontal, T.gutter)
    }
}
