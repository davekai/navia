import SwiftUI

/// The Genres page. The user's top genres show as tiles of their own covers, then every genre as a compact row in
/// the core's order. The filter field narrows the rows by name and hides the tiles.
struct GenresPage: View {
    let shell: ShellModel
    @State private var query = ""
    @Environment(\.pageContext) private var ctx

    var body: some View {
        let page = shell.library.genrePage
        let needle = query.trimmingCharacters(in: .whitespaces).lowercased()
        let rows = needle.isEmpty ? page.all : page.all.filter { $0.name.lowercased().contains(needle) }
        PageFrame(shell: shell, nav: .none, title: L("Genres"), hasRight: !page.all.isEmpty, refresh: .library) {
            SearchPill(placeholder: L("Search Genres"), text: $query, accent: shell.theme.accent, width: pillSearchWidth(ctx))
        } content: {
            PageScroll {
                if page.all.isEmpty {
                    LibraryEmptyState(library: shell.library, symbol: "guitars", title: L("No genres yet"), hint: L("Genres from the server's tags show up here."))
                } else {
                    if needle.isEmpty && !page.top.isEmpty {
                        Shelf(title: L("Your Top Genres"), tiles: true) {
                            ForEach(page.top, id: \.name) { genre in
                                GenreFanTile(genre: genre) { shell.go(.songs(genre: genre.name)) }
                            }
                        }
                    }
                    if rows.isEmpty {
                        EmptyStateView(symbol: "magnifyingglass", title: L("No genres match"), hint: L("Nothing matches “{query}”.", ["query": query]),
                                       compact: true, actionTitle: L("Clear Search")) { query = "" }
                    } else {
                        VStack(alignment: .leading, spacing: T.px(14)) {
                            SectionTitle(title: L("All Genres")).padding(.leading, T.gutter)
                            GenreRows(genres: rows) { shell.go(.songs(genre: $0.name)) }
                        }
                    }
                }
            }
        }
        #if DEBUG
        .onAppear { let binding = $query; GenresDebug.setQuery = { binding.wrappedValue = $0 } }
        #endif
    }
}

#if DEBUG
/// CoreCheck's `genreQuery` and `genreReport`.
@MainActor enum GenresDebug {
    static var setQuery: ((String) -> Void)?
    static var tinted: Set<String> = []
}
#endif

/// A genre's tile, with up to three of its covers fanned in the corner on their tint. A genre without covers gets a
/// plain neutral tile, since the tint is never made up from the name.
struct GenreFanTile: View {
    let genre: GenreEntry
    var width: CGFloat = T.gridCard
    let open: () -> Void
    @State private var hovering = false
    @State private var hex: String?

    private var source: PlaylistCoverSource {
        switch genre.covers.count {
        case 0: .none
        case 1: .image(genre.covers[0])
        default: .grid(genre.covers)
        }
    }

    var body: some View {
        let height = width / T.genreTileRatio
        let tint = hex.map { Tint(hex: $0) }
        Button(action: open) {
            ZStack(alignment: .topLeading) {
                TileFill(tint: tint, width: width, height: height)
                fan(width: width, height: height)
                VStack(alignment: .leading, spacing: T.px(2)) {
                    Text(genre.name).font(.navia(T.textFeature, .semibold)).tracking(-0.012 * T.textFeature)
                        .foregroundStyle(T.tEm.color).lineLimit(2).truncationMode(.tail).fullTextHelp(genre.name)
                    Text(albumCount(genre.albums)).font(.navia(T.textSM)).foregroundStyle((tint?.fgDim ?? T.t2).color).lineLimit(1)
                }
                .padding(.top, width * T.genreTilePadY)
                .padding(.horizontal, width * T.genreTilePadX)
            }
            .frame(width: width, height: height, alignment: .topLeading)
            .clipShape(RoundedRectangle(cornerRadius: T.rMD, style: .continuous))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .task(id: source) {
            if let known = PlaylistTints.shared.cached(source) { hex = known } else { hex = await PlaylistTints.shared.read(source) }
            #if DEBUG
            if let hex { GenresDebug.tinted.insert("\(genre.name):\(hex)") }
            #endif
        }
    }

    /// Covers back to front, the most played on top, each rotated about its bottom center.
    private func fan(width: CGFloat, height: CGFloat) -> some View {
        let side = width * T.genreFanSize
        let cards = genre.covers.prefix(T.genreFan.count)
        return ZStack(alignment: .bottomTrailing) {
            ForEach(Array(cards.enumerated()), id: \.element) { index, url in
                let card = cards.count == 1 ? T.genreFanSingle : (hovering ? T.genreFanHover : T.genreFan)[index]
                ArtworkView(url: url, pixels: Int(side * 2))
                    .frame(width: side, height: side)
                    .clipShape(RoundedRectangle(cornerRadius: T.rSM, style: .continuous))
                    .artShadow()
                    .rotationEffect(.degrees(card.degrees), anchor: .bottom)
                    .offset(x: card.x * side, y: card.y * side)
                    .zIndex(Double(cards.count - index))
            }
        }
        .offset(x: -T.genreFanRight * width, y: -T.genreFanBottom * height)
        .frame(width: width, height: height, alignment: .bottomTrailing)
        .animation(T.ease(T.med), value: hovering)
        .allowsHitTesting(false)
    }
}

/// Every genre as a dense row, several to a line, with one cover, the name and its album count. Rows are lazy, so
/// covers load only near the screen.
struct GenreRows: View {
    let genres: [GenreEntry]
    let open: (GenreEntry) -> Void
    @Environment(\.pageContext) private var ctx

    var body: some View {
        let usable = max(T.genreChipW, (ctx.layoutWidth > 0 ? ctx.layoutWidth : ctx.pageWidth) - 2 * T.gutter)
        let (columns, width) = gridFit(width: usable, cell: T.genreChipW, gap: T.tileGap)
        let lines = (genres.count + columns - 1) / columns
        LazyVStack(alignment: .leading, spacing: 0) {
            ForEach(0..<lines, id: \.self) { line in
                HStack(spacing: T.tileGap) {
                    ForEach(line * columns..<min(line * columns + columns, genres.count), id: \.self) { index in
                        GenreRow(genre: genres[index]) { open(genres[index]) }.frame(width: width)
                    }
                }
            }
        }
        .padding(.horizontal, T.gutter)
    }
}

private struct GenreRow: View {
    let genre: GenreEntry
    let open: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: open) {
            HStack(spacing: T.genreRowGap) {
                ArtworkView(url: genre.covers.first, pixels: Int(T.thumb * 2))
                    .frame(width: T.thumb, height: T.thumb)
                    .clipShape(RoundedRectangle(cornerRadius: T.rXS, style: .continuous))
                VStack(alignment: .leading, spacing: 0) {
                    Text(genre.name).font(.navia(T.textMD, .medium)).foregroundStyle(T.t1.color).lineLimit(2).truncationMode(.tail).fullTextHelp(genre.name)
                    Text(albumCount(genre.albums)).font(.navia(T.textSM)).foregroundStyle(T.t2.color).lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, T.genreRowPad)
            .frame(height: T.miniRowH)
            .background {
                RoundedRectangle(cornerRadius: T.rRow, style: .continuous).fill(hovering ? T.row.color : .clear)
                    .animation(T.hover(hovering), value: hovering)
            }
            .overlay(alignment: .bottom) {
                T.line.color.frame(height: 1)
                    .padding(.leading, T.genreRowPad + T.thumb + T.genreRowGap)
                    .padding(.trailing, T.genreRowPad)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
    }
}
