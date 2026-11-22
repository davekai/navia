import SwiftUI

/// A For You mix, laid out like a playlist page with songs made fresh each day. Nothing is on the server until
/// Save as Playlist.
struct MixPage: View {
    let shell: ShellModel
    let id: String
    @State private var holder = Once<any SongsViewModel>()
    @Environment(\.pageContext) private var ctx
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        if let mix = shell.library.mix(id: id) {
            let songs = holder.get { shell.library.songs(forMix: mix) }
            let header = MixHeader(shell: shell, mix: mix)
                .padding(.leading, ctx.sidebarUnder + T.gutter)
                .padding(.trailing, T.gutter + ctx.dockUnder)
                .padding(.top, PagePadding.top)
                .padding(.bottom, T.sectionGap)
                .background(alignment: .top) { PlaylistPageTint(id: mix.id, source: PlaylistCoverSource(collage: mix.collage)) }
            PageFrame(shell: shell, nav: .icons, edgeFade: true) {
                if isSnapshot {
                    header.frame(maxHeight: .infinity, alignment: .top)
                } else {
                    SongsTable(songs: songs, shell: shell, revision: songs.revision, editRevision: songs.editRevision,
                               nowPlayingID: shell.player.current?.id, pageHeader: AnyView(header),
                               bottomInset: PagePadding.bottom(hasBar: ctx.hasBar),
                               sort: songs.sort, descending: songs.descending, reorder: nil, remove: nil)
                }
            }
        } else if shell.library.forYou() == nil {
            PageFrame(shell: shell, nav: .icons) { PageScroll { EmptyView() } }
        } else {
            PageFrame(shell: shell, nav: .icons) {
                PageScroll { EmptyStateView(symbol: "music.note.list", title: L("This mix isn't here today")) }
            }
        }
    }
}

private struct MixHeader: View {
    let shell: ShellModel
    let mix: MixInfo
    @State private var saving = false

    var body: some View {
        VStack(alignment: .leading, spacing: T.sectionGap + T.px(6)) {
            HStack(alignment: .center, spacing: T.px(25)) {
                PlaylistCover(mix: mix)
                    .frame(width: T.heroArt, height: T.heroArt)
                    .artShadow()
                VStack(alignment: .leading, spacing: 0) {
                    Text(mix.title).font(.navia(T.textTitle, .bold)).foregroundStyle(T.tEm.color).tracking(-0.024 * T.textTitle).lineLimit(2).truncationMode(.tail).fullTextHelp(mix.title)
                    Text("\(songsAndLength(mix.songs.count, seconds: mix.duration)) • \(L("Updated today"))")
                        .font(.navia(T.textMD))
                        .padding(.top, T.px(14))
                    HeaderDescription(about: AboutText(title: mix.title, subtitle: L("Mix"), art: .cover(PlaylistCover(mix: mix)), text: mix.artistsLine), shell: shell)
                        .padding(.top, T.px(14))
                }
            }
            HStack(spacing: T.px(8)) {
                PlayActions(songs: mix.songs, shell: shell) {
                    TextPillButton(symbol: "plus.square.fill", title: L("Save as Playlist"), disabled: saving) { save() }
                }
                Spacer(minLength: 0)
            }
        }
    }

    /// Saves today's songs as a regular playlist. Test runs are refused.
    private func save() {
        saving = true
        let (title, songs) = (mix.title, mix.songs)
        Task { @MainActor in
            do {
                _ = try await shell.library.createPlaylist(name: title, songs: songs)
                shell.showToast(L("Saved as {name}", ["name": title]))
            } catch {
                shell.showToast((error as? LocalizedError)?.errorDescription ?? "\(error)")
            }
            saving = false
        }
    }
}
