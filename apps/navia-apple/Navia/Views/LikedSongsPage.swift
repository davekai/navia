import SwiftUI

/// Every song this user favorited, newest first. Built in, so it isn't a playlist and can't be reordered.
struct LikedSongsPage: View {
    let shell: ShellModel
    @State private var holder = Once<any SongsViewModel>()
    @Environment(\.pageContext) private var ctx
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        let songs = holder.get { shell.library.likedSongs() }
        let all = songs.entireList()
        let header = LikedHeader(shell: shell, songs: songs, tracks: all)
            .padding(.leading, ctx.sidebarUnder + T.gutter)
            .padding(.trailing, T.gutter + ctx.dockUnder)
            .padding(.top, PagePadding.top)
            .padding(.bottom, T.sectionGap)
        PageFrame(shell: shell, nav: .none, edgeFade: true) {
            EmptyView()
        } content: {
            ZStack(alignment: .top) {
                if isSnapshot {
                    header.frame(maxHeight: .infinity, alignment: .top)
                } else if songs.count == 0 {
                    VStack(spacing: 0) {
                        header
                        empty(songs)
                    }
                    .frame(maxHeight: .infinity, alignment: .top)
                } else {
                    SongsTable(songs: songs, shell: shell, revision: songs.revision, editRevision: songs.editRevision,
                               nowPlayingID: shell.player.current?.id, pageHeader: AnyView(header),
                               bottomInset: PagePadding.bottom(hasBar: ctx.hasBar), sort: songs.sort, descending: songs.descending)
                }
            }
        }
    }

    @ViewBuilder private func empty(_ songs: any SongsViewModel) -> some View {
        if !songs.query.isEmpty {
            EmptyStateView(symbol: "magnifyingglass", title: L("No songs match"), hint: L("Nothing in Liked Songs matches “{query}”.", ["query": songs.query]), compact: true, actionTitle: L("Clear")) { songs.query = "" }
        } else if songs.isLoading {
            Color.clear.frame(height: T.px(240))
        } else {
            LibraryEmptyState(library: shell.library, symbol: "heart", title: L("No liked songs yet"), hint: L("Songs you favorite with the heart show up here, newest first."))
        }
    }
}

private struct LikedHeader: View {
    let shell: ShellModel
    let songs: any SongsViewModel
    let tracks: [SongRow]
    @Environment(\.pageContext) private var ctx

    var body: some View {
        VStack(alignment: .leading, spacing: T.sectionGap + T.px(6)) {
            CoverHead(title: L("Liked Songs")) {
                PlaylistCover.likedSongs
            } meta: {
                Text(songsAndLength(tracks.count, seconds: tracks.reduce(0) { $0 + $1.duration })).font(.navia(T.textMD))
                HeaderDescription(about: AboutText(title: L("Liked Songs"), art: .cover(.likedSongs), text: L("Every song you've liked, newest first.")), shell: shell)
            }
            HStack(spacing: T.px(8)) {
                PlayActions(songs: tracks, shell: shell) { EmptyView() }
                Spacer(minLength: 0)
                GlassGroup {
                    SortButton(symbol: "arrow.up.arrow.down", label: L("Sort"), options: SongSort.likedSorts, title: \.label, current: songs.sort) { songs.sort = $0 }
                }
                SearchPill(placeholder: L("Search Songs"), text: Binding(get: { songs.query }, set: { songs.query = $0 }), accent: shell.theme.accent, width: pillSearchWidth(ctx))
            }
        }
    }
}
