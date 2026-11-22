import NaviaCoreFFI
import SwiftUI

/// Every library song, narrowed by the Songs filters. From a genre tile it opens with that genre picked.
struct SongsPage: View {
    let shell: ShellModel
    let genre: String?
    @State private var holder = Once<any SongsViewModel>()
    @State private var filtersHolder = Once<FiltersModel>()
    @Environment(\.pageContext) private var ctx
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        let songs = holder.get { genre.map { shell.library.songs(forGenre: $0) } ?? shell.songs }
        let filters = filtersHolder.get {
            let model = FiltersModel(core: (shell.library as? CoreLibraryModel)?.core, page: .songs, genre: genre)
            let list = songs as? CoreSongsModel
            list?.filterSettings = model.settings
            model.onChange = { [weak list] settings in list?.filterSettings = settings }
            return model
        }
        let narrowed = !songs.query.isEmpty || filters.isFiltering
        let row = filters.available.isEmpty ? nil : FilterRow(shell: shell, filters: filters, offersSave: true, save: save(songs, filters))
            .padding(.leading, ctx.sidebarUnder + T.gutter)
            .padding(.trailing, T.gutter + ctx.dockUnder)
            .padding(.top, T.topbarH + T.chipGap)
            .padding(.bottom, T.filterRowGap)
        PageFrame(shell: shell, nav: .none, title: L("Songs"), detail: narrowed ? Ln("{count} match", "{count} matches", songs.count) : Ln("{count} song", "{count} songs", songs.totalCount), edgeFade: true, refresh: .library) {
            GlassGroup {
                SortButton(symbol: "arrow.up.arrow.down", label: L("Sort"), options: SongSort.librarySorts, title: \.label, current: songs.sort) { songs.sort = $0 }
            }
            GlassGroup {
                IconButton(symbol: "list.bullet", label: L("Comfortable rows"), active: !shell.compactRows, inGlass: true) { shell.compactRows = false }
                IconButton(symbol: "line.3.horizontal", label: L("Compact rows"), active: shell.compactRows, inGlass: true) { shell.compactRows = true }
            }
            SearchPill(placeholder: L("Search Songs"), text: Binding(get: { songs.query }, set: { songs.query = $0 }), accent: shell.theme.accent, width: pillSearchWidth(ctx))
        } content: {
            Group {
                if songs.count == 0 && !isSnapshot {
                    VStack(spacing: 0) {
                        if let row { row }
                        empty(songs, filters: filters)
                            .padding(.top, row == nil ? PagePadding.top : 0)
                            .padding(.leading, ctx.sidebarUnder)
                            .padding(.trailing, ctx.dockUnder)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
                } else if !isSnapshot {
                    SongsTable(songs: songs, shell: shell, revision: songs.revision, editRevision: songs.editRevision,
                               nowPlayingID: shell.player.current?.id, compact: shell.compactRows, pageHeader: row.map { AnyView($0) },
                               bottomInset: PagePadding.bottom(hasBar: ctx.hasBar), sort: songs.sort, descending: songs.descending)
                } else {
                    Color.clear
                }
            }
            .task(id: songs.totalCount) { await filters.load() }
        }
    }

    /// Save as Smart Playlist: opens the smart editor prefilled with the filters and sort. Nil while a typed range is
    /// unfinished.
    private func save(_ songs: any SongsViewModel, _ filters: FiltersModel) -> (() -> Void)? {
        guard !filters.unfinished, let list = songs as? CoreSongsModel else { return nil }
        return { [shell] in
            let rules = filters.rules(order: list.orderBy)
            shell.smartPrefill = (rules.json, rules.dropped)
            shell.popups.close()
            shell.go(.smartEditor(nil))
        }
    }

    /// Says when nothing fits the search and filters, with Clear All. Otherwise the library's own empty state.
    @ViewBuilder private func empty(_ songs: any SongsViewModel, filters: FiltersModel) -> some View {
        if filters.isFiltering || !songs.query.isEmpty {
            let hint = !filters.isFiltering ? L("Nothing here matches “{query}”.", ["query": songs.query])
                : songs.query.isEmpty ? L("No songs fit these filters.") : L("Nothing matches “{query}” with these filters.", ["query": songs.query])
            EmptyStateView(symbol: "magnifyingglass", title: L("No songs match"), hint: hint, actionTitle: L("Clear All")) {
                songs.query = ""
                filters.clear()
            }
        } else if songs.isLoading {
            Color.clear.frame(height: T.px(240))
        } else {
            LibraryEmptyState(library: shell.library, symbol: "music.note", title: L("No songs yet"), hint: L("Songs on the server show up here."))
        }
    }
}
