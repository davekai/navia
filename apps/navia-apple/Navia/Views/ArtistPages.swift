import SwiftUI

/// Every artist as a grid or a split view, narrowed by the filter row and the page search together.
struct ArtistsPage: View {
    let shell: ShellModel
    @State private var holder = Once<any ArtistsViewModel>()
    @State private var filtersHolder = Once<FiltersModel>()
    @State private var any = false
    @State private var selected: String?
    @AppStorage("artistLayout") private var layout = PageLayout.grid.rawValue
    @Environment(\.pageContext) private var ctx

    var body: some View {
        let artists = holder.get { shell.library.artists() }
        let filters = filtersHolder.get {
            let model = FiltersModel(core: (shell.library as? CoreLibraryModel)?.core, page: .artists)
            model.onChange = { [weak artists] settings in artists?.filterSettings = settings }
            return model
        }
        let narrowed = !artists.query.isEmpty || filters.isFiltering
        PageFrame(shell: shell, nav: .none, title: L("Artists"), detail: narrowed ? Ln("{count} match", "{count} matches", artists.count) : nil, hasRight: any, refresh: .library) {
            LayoutToggle(layout: $layout)
            SearchPill(placeholder: L("Search Artists"), text: Binding(get: { artists.query }, set: { artists.query = $0 }), accent: shell.theme.accent, width: pillSearchWidth(ctx))
        } content: {
            Group {
                if layout == PageLayout.split.rawValue && artists.count > 0 {
                    split(artists, filters: filters)
                } else {
                    PageScroll {
                        // The row and what it narrows are one section, so the row's gaps stay apart from the page's section gap.
                        VStack(alignment: .leading, spacing: 0) {
                            filterRow(filters).padding(.top, T.chipGap - T.contentTop)
                            results(artists, filters: filters)
                        }
                    }
                }
            }
            .task(id: any) { await filters.load() }
        }
        // The search shows once the library has artists, and stays while it narrows them.
        .onChange(of: artists.count > 0, initial: true) { _, has in if has { any = true } }
    }

    @ViewBuilder private func filterRow(_ filters: FiltersModel) -> some View {
        if !filters.available.isEmpty {
            FilterRow(shell: shell, filters: filters)
                .padding(.horizontal, T.gutter)
                .padding(.bottom, T.filterRowGap)
        }
    }

    /// The filter row, then the shown artists beside the chosen one's page. With none chosen, or the choice filtered
    /// out, the first artist shows.
    private func split(_ artists: any ArtistsViewModel, filters: FiltersModel) -> some View {
        let current = selected.flatMap { id in (0..<artists.count).lazy.compactMap { artists.artist(at: $0) }.first { $0.id == id } } ?? artists.artist(at: 0)
        return VStack(alignment: .leading, spacing: 0) {
            filterRow(filters)
                .padding(.top, T.topbarH + T.chipGap)
                .padding(.leading, ctx.sidebarUnder)
                .padding(.trailing, ctx.dockUnder)
            SplitView(top: 0) {
                ForEach(0..<artists.count, id: \.self) { index in
                    if let artist = artists.artist(at: index) {
                        let counts = artists.counts(of: artist.id)
                        SplitItem(on: artist.id == current?.id, title: artist.name,
                                  subtitle: counts.map { "\(albumCount(Int($0.albums))) • \(songCount(Int($0.songs)))" } ?? "",
                                  action: { selected = artist.id }) {
                            ArtworkView(url: artist.photoURL, kind: .artist, pixels: Int(T.splitArt * 2)).clipShape(Circle())
                        }
                    }
                }
            } detail: {
                if let current {
                    ArtistDetail(shell: shell, artist: current, embedded: true, scrollTop: 0).id(current.id)
                }
            }
        }
    }

    @ViewBuilder private func results(_ artists: any ArtistsViewModel, filters: FiltersModel) -> some View {
        if artists.count == 0 && filters.isFiltering {
            EmptyStateView(symbol: "magnifyingglass", title: L("No artists match"),
                           hint: artists.query.isEmpty ? L("No artists fit these filters.") : L("Nothing matches “{query}” with these filters.", ["query": artists.query]),
                           actionTitle: L("Clear All")) {
                artists.query = ""
                filters.clear()
            }
        } else if artists.count == 0 && !artists.query.isEmpty {
            EmptyStateView(symbol: "magnifyingglass", title: L("No artists match"), hint: L("Nothing here matches “{query}”.", ["query": artists.query]),
                           actionTitle: L("Clear Search")) { artists.query = "" }
        } else if artists.count == 0 {
            LibraryEmptyState(library: shell.library, symbol: "music.microphone", title: L("No artists"), hint: L("Artists on the server show up here."))
        } else {
            LazyCardGrid(count: artists.count) { index, width in
                if let artist = artists.artist(at: index) {
                    ArtistCard(artist: artist, width: width) { shell.go(.artist(artist.id)) }
                }
            }
        }
    }
}

struct ArtistPage: View {
    let shell: ShellModel
    let id: String

    var body: some View {
        if let artist = shell.library.artist(id: id) {
            PageFrame(shell: shell, nav: .icons) { ArtistDetail(shell: shell, artist: artist).id(artist.id) }
        } else {
            PageFrame(shell: shell) {
                PageScroll { EmptyStateView(symbol: "music.microphone", title: L("This artist isn't in your library")) }
            }
        }
    }
}

/// One artist's page without its frame. `embedded` is the split view's version, with no glow.
struct ArtistDetail: View {
    let shell: ShellModel
    let artist: ArtistRow
    var embedded = false
    /// `PageScroll.top`, set when the page sits below other content.
    var scrollTop: CGFloat? = nil
    @State private var query = ""
    @Environment(\.pageContext) private var ctx

    var body: some View {
        let library = shell.library
        let needle = query.trimmingCharacters(in: .whitespaces).lowercased()
        // Nil while the server's list is loading, so neither albums nor "nothing here" show.
        let discography = library.discography(for: artist)
        let loading = discography == nil
        let parts = (discography ?? []).map { part in
            DiscographyPart(section: part.section, albums: needle.isEmpty ? part.albums : part.albums.filter { $0.name.lowercased().contains(needle) })
        }.filter { !$0.albums.isEmpty }
        let top = library.topSongs(for: artist)
        let songs = needle.isEmpty ? Array(top.prefix(6)) : library.songs(by: artist, matching: needle)
        // The newest of the artist's own releases. Each section is newest first.
        let latest = needle.isEmpty ? Self.latest(discography ?? []) : nil
        return PageScroll(glow: embedded ? nil : artist.photoURL, top: scrollTop) {
            header(artist, top: top)
            if !songs.isEmpty || latest != nil {
                columns(songs: songs, latest: latest, needle: needle)
            }
            if needle.isEmpty { UpcomingShelf(albums: library.upcoming(for: artist), shell: shell) }
            ForEach(parts) { part in
                Shelf(title: part.section.title, action: needle.isEmpty ? { shell.openAlbums(.artist(artist.id, part.section), sort: .year) } : nil) {
                    let shared = part.albums.sharedNames
                    ForEach(part.albums) { album in AlbumCard(album: album, shell: shell, showEdition: shared.contains(album.name.lowercased())) { shell.go(.album(album.id)) } }
                }
            }
            // Similar Artists last, once answered. Hidden when empty.
            if needle.isEmpty {
                let similar = library.similarArtists(for: artist)
                if !similar.isEmpty {
                    Shelf(title: L("Similar Artists")) {
                        ForEach(similar) { other in ArtistCard(artist: other) { shell.go(.artist(other.id)) } }
                    }
                }
            }
            if !needle.isEmpty && songs.isEmpty && parts.isEmpty && !loading {
                EmptyStateView(symbol: "magnifyingglass", title: L("No matches"), hint: L("Nothing by {name} matches “{query}”.", ["name": artist.name, "query": query]), compact: true, actionTitle: L("Clear Search")) { query = "" }
            }
            if needle.isEmpty && top.isEmpty && (discography ?? []).isEmpty && !loading {
                EmptyStateView(symbol: "music.microphone", title: L("Nothing here yet"), hint: L("The server has no songs or albums for this artist."), compact: true)
            }
        }
        .padding(.trailing, ctx.end)
    }

    private static func latest(_ parts: [DiscographyPart]) -> AlbumRow? {
        parts.filter { $0.section != .appearsOn }.compactMap(\.albums.first).max { ($0.year ?? 0) < ($1.year ?? 0) }
    }

    /// The header, with the uncropped photo beside the name and controls, over the page glow.
    private func header(_ artist: ArtistRow, top: [SongRow]) -> some View {
        let library = shell.library
        let radio = library.similarSongs(for: artist)
        let current = library.artist(id: artist.id) ?? artist
        let info = VStack(alignment: .leading, spacing: T.artistHeadTitleGap) {
            Text(artist.name)
                .font(.navia(T.textTitle, .bold)).tracking(-0.026 * T.textTitle)
                .foregroundStyle(T.tEm.color)
                .lineLimit(2).truncationMode(.tail)
                .fullTextHelp(artist.name)
                .shadow(T.heroTitleShadow)
            actionBar(artist, current: current, radio: radio)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        return Group {
            if ctx.cssWidth <= 620 {
                VStack(alignment: .leading, spacing: T.artistHeadGap) { ArtistPhoto(url: artist.photoURL); info }
            } else {
                HStack(alignment: .bottom, spacing: T.artistHeadGap) { ArtistPhoto(url: artist.photoURL); info }
            }
        }
        .padding(.horizontal, ctx.gutter)
    }

    private func actionBar(_ artist: ArtistRow, current: ArtistRow, radio: [SongRow]) -> some View {
        let library = shell.library
        return ActionRow(spacing: T.px(10), pushTrailing: ctx.cssWidth > 900) {
            PlayActions(songs: library.playableSongs(for: artist), shell: shell) {
                IconButton(symbol: "heart", label: L("Favorite"), active: current.isFavorite, inGlass: true, pop: true) { shell.library.toggleFavorite(artist: artist.id) }
                if ArtistAbout.of(current, shell: shell) != nil {
                    IconButton(symbol: "info.circle", label: L("About {name}", ["name": artist.name]), inGlass: true) { shell.openAbout(artist.id) }
                }
                MenuIconButton(symbol: "ellipsis", label: L("More")) {
                    [MenuEntry(title: L("Artist Radio"), enabled: !radio.isEmpty) { shell.startArtistRadio(artist) },
                     shell.shareEntry(artist: current),
                     SongMenu.info(.artist(artist.id), shell: shell)]
                }
            }
        } trailing: {
            SearchPill(placeholder: L("Search Artist"), text: $query, accent: shell.theme.accent, width: pillSearchWidth(ctx))
        }
    }

    /// Top Songs and Latest Release in two equal columns, or stacked when narrow.
    private func columns(songs: [SongRow], latest: AlbumRow?, needle: String) -> some View {
        let stacked = ctx.cssWidth <= 900 || latest == nil || songs.isEmpty
        return Group {
            if stacked {
                VStack(alignment: .leading, spacing: T.artistColumnsStackGap) { songsColumn(songs, needle); latestColumn(latest) }
            } else {
                HStack(alignment: .top, spacing: T.artistColumnsGap) {
                    songsColumn(songs, needle).frame(maxWidth: .infinity, alignment: .topLeading)
                    latestColumn(latest).frame(maxWidth: .infinity, alignment: .topLeading)
                }
            }
        }
        .padding(.horizontal, ctx.gutter)
    }

    @ViewBuilder private func songsColumn(_ songs: [SongRow], _ needle: String) -> some View {
        if !songs.isEmpty {
            VStack(alignment: .leading, spacing: T.px(12)) {
                SectionTitle(title: needle.isEmpty ? L("Top Songs") : L("Songs"), action: needle.isEmpty ? { shell.go(.artistTop(artist.id)) } : nil)
                SongColumnsView(songs: songs, shell: shell, one: true, gutter: 0)
            }
        }
    }

    @ViewBuilder private func latestColumn(_ latest: AlbumRow?) -> some View {
        if let latest {
            VStack(alignment: .leading, spacing: T.px(14)) {
                SectionTitle(title: L("Latest Release"))
                Button { shell.go(.album(latest.id)) } label: {
                    HStack(alignment: .top, spacing: T.px(25)) {
                        ArtworkView(url: latest.artworkURL, pixels: 400)
                            .frame(width: T.cardW, height: T.cardW)
                            .clipShape(RoundedRectangle(cornerRadius: T.rMD, style: .continuous))
                        VStack(alignment: .leading, spacing: 0) {
                            if let year = latest.year { Text(String(year)).font(.navia(T.textMD, .semibold)).foregroundStyle(T.t2.color).padding(.bottom, T.px(8)) }
                            Text(latest.name).font(.navia(T.textMD, .medium))
                            Text(songCount(latest.songCount)).font(.navia(T.textMD)).foregroundStyle(T.t2.color)
                        }
                        .padding(.top, T.px(2))
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
    }
}

/// The artist's photo uncropped, `heroArt` tall, up to `artistArtMaxRatio` wide, rounded and shadowed. Square while
/// loading, and a placeholder without one.
struct ArtistPhoto: View {
    let url: URL?
    @State private var aspect: CGFloat = 1

    var body: some View {
        ArtworkView(url: url, kind: .artist, pixels: 600)
            .frame(width: T.heroArt * min(aspect, T.artistArtMaxRatio), height: T.heroArt)
            .clipShape(RoundedRectangle(cornerRadius: T.rLG, style: .continuous))
            .shadow(T.shadowArt)
            .task(id: url) {
                guard let url, let image = await ArtworkLoader.shared.load(url, pixels: 600), image.size.height > 0 else { aspect = 1; return }
                aspect = image.size.width / image.size.height
            }
    }
}

/// the server's top songs for an artist that are in the library, in the server's order.
struct TopSongsPage: View {
    let shell: ShellModel
    let id: String

    var body: some View {
        if let artist = shell.library.artist(id: id) {
            let top = shell.library.topSongs(for: artist)
            PageFrame(shell: shell, nav: .icons, edgeFade: true) { TopSongsList(shell: shell, artist: artist, top: top).id(top.map(\.id)) }
        } else {
            PageFrame(shell: shell) {
                PageScroll { EmptyStateView(symbol: "music.microphone", title: L("This artist isn't in your library")) }
            }
        }
    }
}

/// rebuilt when the server's answer arrives, so its model holds the full list.
private struct TopSongsList: View {
    let shell: ShellModel
    let artist: ArtistRow
    let top: [SongRow]
    @State private var holder = Once<any SongsViewModel>()
    @Environment(\.pageContext) private var ctx
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        let songs = holder.get { shell.library.songs(forArtistTop: top) }
        let summary = songsAndLength(top.count, seconds: top.reduce(0) { $0 + $1.duration })
        let header = VStack(alignment: .leading, spacing: T.sectionGap + T.px(6)) {
            CoverHead(title: L("Top Songs")) {
                ArtworkView(url: artist.photoURL, kind: .artist, pixels: 600)
                    .clipShape(RoundedRectangle(cornerRadius: T.rMD, style: .continuous))
            } meta: {
                LinkedText(pieces: [LinkPiece(id: 0, text: artist.name, route: .artist(artist.id)), LinkPiece(id: 1, text: " • \(summary)", route: nil)], shell: shell)
                    .font(.navia(T.textMD))
            }
            HStack(spacing: T.px(8)) {
                PlayActions(songs: top, shell: shell) { EmptyView() }
                Spacer(minLength: 0)
            }
        }
        .padding(.leading, ctx.sidebarUnder + T.gutter)
        .padding(.trailing, T.gutter + ctx.dockUnder)
        .padding(.top, PagePadding.top)
        .padding(.bottom, T.sectionGap)
        .background(alignment: .top) { if let url = artist.photoURL { PageGlow(url: url, windowHeight: ctx.windowHeight) } }
        if isSnapshot || top.isEmpty {
            header.frame(maxHeight: .infinity, alignment: .top)
        } else {
            SongsTable(songs: songs, shell: shell, revision: songs.revision, editRevision: songs.editRevision,
                       nowPlayingID: shell.player.current?.id, pageHeader: AnyView(header),
                       bottomInset: PagePadding.bottom(hasBar: ctx.hasBar), sort: nil)
        }
    }
}
