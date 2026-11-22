import SwiftUI

/// the sidebar's fixed pages, also the Go menu.
enum SidebarPages {
    static var top: [(Route, String, String)] {
        [(.browse(), L("Search"), "magnifyingglass"), (.home, L("Home"), "house"), (.discover(.forYou), L("For You"), "sparkle"),
         (.discover(.new), L("New"), "square.grid.2x2"), (.discover(.radio), L("Radio"), "dot.radiowaves.left.and.right")]
    }
    static var library: [(Route, String, String)] {
        [(.liked, L("Liked Songs"), "heart"), (.songs(genre: nil), L("Songs"), "music.note"),
         (.artists, L("Artists"), "music.microphone"), (.albums(.all), L("Albums"), "square.stack"), (.browse(genres: true), L("Genres"), "guitars")]
    }
}

struct SidebarView: View {
    let shell: ShellModel
    let width: CGFloat
    let toggle: () -> Void
    @State private var libraryOpen = true
    @State private var playlistsOpen = true
    @State private var sharedOpen = true
    @State private var scrollPosition = ScrollPosition(y: 0)
    @State private var scrollTop: CGFloat = 0
    @State private var scrollHeight: CGFloat = 0
    @State private var contentHeight: CGFloat = 0
    @Environment(\.isSnapshot) private var isSnapshot

    private let top = SidebarPages.top
    private let library = SidebarPages.library

    var body: some View {
        VStack(spacing: 0) {
            if isSnapshot {
                header
                rows.frame(minHeight: 0, maxHeight: .infinity, alignment: .top).clipped()
            } else {
                // Rows blur and fade as they reach the window buttons. The header acts as the bar, so rows start below it.
                ScrollView(.vertical) { rows.coordinateSpace(.named(SidebarPlaylists.space)) }
                    .scrollPosition($scrollPosition)
                    .onScrollGeometryChange(for: CGRect.self, of: { CGRect(x: 0, y: $0.contentOffset.y, width: $0.containerSize.height, height: $0.contentSize.height) }) { _, g in
                        scrollTop = g.minY
                        scrollHeight = g.width
                        contentHeight = g.height
                        SidebarPlaylists.scrollTo = { y in scrollPosition.scrollTo(y: y) }
                    }
                    // Songs carried near the top or bottom edge scroll it. Scrolling goes through the scroll position so it stays put.
                    .onScrollGeometryChange(for: [CGFloat].self, of: { g in
                        [g.contentOffset.y + g.contentInsets.top, max(0, g.contentSize.height - g.containerSize.height + g.contentInsets.top + g.contentInsets.bottom)]
                    }) { _, at in
                        shell.songDrag.sidebarScrollBy = { delta in scrollPosition.scrollTo(y: min(at[1], max(0, at[0] + delta))) }
                    }
                    .scrollIndicators(.never)
                    .scrollEdgeEffectStyle(.soft, for: .top)
                    .safeAreaBar(edge: .top, spacing: 0) { header }
            }

            // Settings lives in the account button, so the playlists end the sidebar.
            if let progress = shell.library.syncProgress, !shell.library.syncMeterOnPage {
                SyncCard(progress: progress, fill: shell.theme.syncFill).padding([.horizontal, .top], T.px(12)).padding(.top, -T.px(6)).padding(.bottom, T.px(12))
            }
            if shell.library.isOffline { OfflineRow(shell: shell).padding([.horizontal, .top], T.px(12)).padding(.top, -T.px(6)).padding(.bottom, T.px(12)) }
        }
        .frame(width: width)
        .frame(maxHeight: .infinity)
        // The scroll view's soft top edge reaches the window's top, so clip it to the panel. Unclipped, it lightened the
        // margin above the panel and showed as a line along the corner.
        .clipShape(RoundedRectangle(cornerRadius: T.rLG, style: .continuous))
        .panelGlass(radius: T.rLG)
    }

    /// The sidebar toggle's row, which also clears the window buttons.
    private var header: some View {
        HStack {
            Spacer()
            IconButton(symbol: "sidebar.left", label: L("Hide sidebar"), action: toggle)
        }
        .padding(.trailing, T.px(12))
        .frame(height: T.px(62))
    }

    private var rows: some View {
        VStack(spacing: 0) {
            ForEach(top, id: \.1) { item(route: $0.0, label: $0.1, symbol: $0.2) }
            heading(L("Library"), open: $libraryOpen)
            if libraryOpen { ForEach(library, id: \.1) { item(route: $0.0, label: $0.1, symbol: $0.2) } }
            // Listening Now shows others who share while this listener shares too, then Spotify friends who are listening.
            if let listen = shell.player.listen {
                let people = listen.active ? listen.listing : []
                let friends = listen.listeningFriends
                if !people.isEmpty || !friends.isEmpty {
                    headingLine(L("Listening Now")) { EmptyView() }
                    ForEach(people, id: \.user) { PersonRow(entry: $0, shell: shell) }
                    ForEach(friends, id: \.id) { FriendRow(friend: $0, shell: shell) }
                }
            }
            heading(L("Playlists"), open: $playlistsOpen)
            if playlistsOpen {
                item(route: .playlists, label: L("All Playlists"), symbol: "music.note.list")
                SidebarPlaylists(shell: shell, section: .mine, scrollTop: $scrollTop, scrollHeight: $scrollHeight, contentHeight: $contentHeight)
            }
            // hidden when there are none, as on a single-user server.
            if shell.library.playlists.contains(where: { $0.section == .shared && shell.sidebarPins.isPinned($0) }) {
                heading(L("Shared||playlists"), open: $sharedOpen)
                if sharedOpen {
                    SidebarPlaylists(shell: shell, section: .shared, scrollTop: $scrollTop, scrollHeight: $scrollHeight, contentHeight: $contentHeight)
                }
            }
        }
        .padding(.horizontal, T.px(12))
        .padding(.top, T.sidebarListTop)
        .padding(.bottom, T.px(12))
        .background(ScrollViewProbe { [songDrag = shell.songDrag] in songDrag.sidebarScroll = $0 })
    }

    private func item(route: Route, label: String, symbol: String) -> some View {
        let on: Bool = {
            if case .search = shell.route, case .browse(genres: false) = route { return true }
            return shell.route == route
        }()
        return SidebarRow(label: label, on: on) {
            Image(systemName: symbol)
                .font(.system(size: T.px(18)))
                .foregroundStyle((on ? T.tEm : T.tNav).color)
                .frame(width: T.px(22))
        } action: { shell.go(route) }
    }

    /// A heading's line, with the title and an optional item at its end.
    private func headingLine(_ title: String, @ViewBuilder end: () -> some View) -> some View {
        HStack {
            Text(title).font(.navia(T.textSM, .semibold)).foregroundStyle(T.tNavSection.color)
            Spacer()
            end()
        }
        .padding(.top, T.sidebarHeadingTop)
        .padding(.bottom, T.sidebarHeadingPadY)
        .padding(.leading, T.px(14))
        .padding(.trailing, T.px(12))
    }

    private func heading(_ title: String, open: Binding<Bool>) -> some View {
        SidebarHeading(title: title, open: open)
    }
}

/// the whole row folds its group; the chevron shows which way.
private struct SidebarHeading: View {
    let title: String
    @Binding var open: Bool
    @State private var hovering = false

    var body: some View {
        let label = open ? L("Collapse {section}", ["section": title]) : L("Expand {section}", ["section": title])
        Button { open.toggle() } label: {
            HStack {
                Text(title).font(.navia(T.textSM, .semibold)).foregroundStyle(T.tNavSection.color)
                Spacer()
                Image(systemName: open ? "chevron.down" : "chevron.right").font(.system(size: T.px(9), weight: .bold))
                    .foregroundStyle((hovering ? T.t1 : T.tNavSection).color)
            }
            .padding(.vertical, T.sidebarHeadingPadY)
            .padding(.leading, T.px(14))
            .padding(.trailing, T.px(12))
            .background(RoundedRectangle(cornerRadius: T.rMD, style: .continuous).fill(hovering ? T.row.color : .clear))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .padding(.top, T.sidebarHeadingTop - T.sidebarHeadingPadY)
        .help(label)
        .accessibilityLabel(label)
    }
}

/// The server can't be reached. Sits at the sidebar's foot with Retry.
private struct OfflineRow: View {
    let shell: ShellModel
    @State private var retrying = false

    var body: some View {
        HStack(spacing: T.px(8)) {
            Image(systemName: "icloud.slash.fill").font(.system(size: T.px(20))).foregroundStyle(T.tNav.color)
            VStack(alignment: .leading, spacing: 0) {
                Text(L("Offline")).font(.navia(T.textMD, .medium))
                Text(L("Downloads only")).font(.navia(T.textXS)).foregroundStyle(T.tNav.color).lineLimit(1).minimumScaleFactor(0.85)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            // the switch is the way back, so no Retry while it's on
            if !shell.library.offlineSwitch {
                Button(action: retry) {
                    Text(L("Retry")).font(.navia(T.textXS, .medium))
                        .padding(.horizontal, T.px(9))
                        .frame(height: T.px(30))
                        .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .disabled(retrying)
                .opacity(retrying ? 0.5 : 1)
                .glassPill()
            }
        }
        .padding(.horizontal, T.px(10))
        .padding(.vertical, T.px(10))
        .background(T.row.color, in: RoundedRectangle(cornerRadius: T.rMD, style: .continuous))
    }

    private func retry() {
        retrying = true
        shell.player.retryServer()
        Task {
            let ok = await shell.library.retryConnection()
            retrying = false
            if !ok { shell.showToast(L("Still can't reach the server")) }
        }
    }
}

/// One group's playlist rows, either the user's own or other people's public ones. Dragging a row reorders it
/// within its group, and the order is saved on this device.
private struct SidebarPlaylists: View {
    let shell: ShellModel
    let section: PlaylistSection
    @Binding var scrollTop: CGFloat
    @Binding var scrollHeight: CGFloat
    @Binding var contentHeight: CGFloat

    static let space = "sidebar-content"
    /// Set by the scroll view, so the drag can auto-scroll it.
    @MainActor static var scrollTo: (CGFloat) -> Void = { _ in }

    @State private var saved: [String] = []
    @State private var loadedKey = ""
    @State private var dragged: String?
    @State private var pointer: CGFloat = 0
    @State private var grab: CGFloat = 0
    @State private var listTop: CGFloat = 0
    @State private var ignoreClick = false

    private var rowH: CGFloat { T.px(45) }
    private var members: [PlaylistRef] {
        let pins = shell.sidebarPins
        return shell.library.playlists.filter { $0.section == section && pins.isPinned($0) }
    }
    private var ordered: [PlaylistRef] { SidebarOrder.merged(saved: saved, current: members) }
    private var gap: Int { max(0, min(ordered.count, Int(((pointer - listTop - grab) / rowH).rounded()))) }

    var body: some View {
        let list = ordered
        let line = dragged.flatMap { id in
            list.firstIndex { $0.id == id }.flatMap { from in gap == from || gap == from + 1 ? nil : gap }
        }
        VStack(spacing: 0) {
            ForEach(list) { row in
                // The user's own public playlists get a quiet mark, since others on the server can see them.
                SidebarRow(label: row.name, on: shell.route == .playlist(row.id), mark: section == .mine && row.isPublic,
                           download: AnyView(DownloadMark(kind: .playlist, id: row.id, shell: shell))) {
                    PlaylistCover(ref: row, small: true).frame(width: T.sidebarCover, height: T.sidebarCover)
                } action: {
                    guard !ignoreClick else { return }
                    shell.go(.playlist(row.id))
                }
                .overlay(alignment: .leading) { CoverPlay(row: row, shell: shell).padding(.leading, T.sidebarItemPadX).allowsHitTesting(dragged == nil) }
                .rightClickMenu { shell.library.playlist(id: row.id).map { PlaylistMenu.entries($0, shell: shell) } ?? [] }
                .opacity(dragged == row.id ? 0.35 : 1)
                .simultaneousGesture(drag(row))
                // Songs dragged here are added.
                .songDropTarget(.playlist(row, place: "sidebar"), shell: shell)
            }
        }
        .overlay(alignment: .topLeading) {
            if let line {
                RoundedRectangle(cornerRadius: 1).fill(shell.theme.accent.color)
                    .frame(height: 2)
                    .padding(.horizontal, T.px(8))
                    .offset(y: CGFloat(line) * rowH - 1)
                    .allowsHitTesting(false)
            }
        }
        .overlay(alignment: .topLeading) {
            if let id = dragged, let row = list.first(where: { $0.id == id }) {
                SidebarRow(label: row.name, on: false) {
                    PlaylistCover(ref: row, small: true).frame(width: T.sidebarCover, height: T.sidebarCover)
                } action: {}
                    .background(T.panel2.color, in: RoundedRectangle(cornerRadius: T.rMD, style: .continuous))
                    .shadow(T.shadowGhost)
                    .offset(y: pointer - listTop - grab)
                    .allowsHitTesting(false)
            }
        }
        .onGeometryChange(for: CGFloat.self) { $0.frame(in: .named(Self.space)).minY } action: { listTop = $0 }
        .task(id: shell.library.accountKey) {
            saved = SidebarOrder.load(account: shell.library.accountKey)
            loadedKey = shell.library.accountKey
        }
        .task(id: dragged) { await autoScroll() }
    }

    private func drag(_ row: PlaylistRef) -> some Gesture {
        DragGesture(minimumDistance: 6, coordinateSpace: .named(Self.space))
            .onChanged { value in
                if dragged == nil {
                    dragged = row.id
                    let index = ordered.firstIndex { $0.id == row.id } ?? 0
                    grab = value.startLocation.y - listTop - CGFloat(index) * rowH
                }
                pointer = value.location.y
            }
            .onEnded { _ in
                drop()
                // The press that ended the drag can still count as a click on the row.
                ignoreClick = true
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) { ignoreClick = false }
            }
    }

    private func drop() {
        guard let id = dragged else { return }
        var list = ordered
        if let from = list.firstIndex(where: { $0.id == id }), SidebarOrder.move(&list, from: from, toGap: gap) {
            // The other group keeps its place in the shared list, read fresh in case it moved.
            let account = shell.library.accountKey
            // the other group and unpinned playlists keep their places after this one.
            let rest = SidebarOrder.merged(saved: SidebarOrder.load(account: account), current: shell.library.playlists).map(\.id).filter { id in !list.contains { $0.id == id } }
            saved = list.map(\.id) + rest
            SidebarOrder.save(saved, account: account)
        }
        dragged = nil
    }

    private func autoScroll() async {
        guard dragged != nil else { return }
        let edge = T.px(36), speed = T.px(10)
        while !Task.isCancelled {
            let inView = pointer - scrollTop
            var delta: CGFloat = 0
            if inView < edge { delta = -speed * min(1, (edge - inView) / edge) }
            else if inView > scrollHeight - edge { delta = speed * min(1, (inView - (scrollHeight - edge)) / edge) }
            if delta != 0 {
                let target = max(0, min(max(0, contentHeight - scrollHeight), scrollTop + delta))
                if target != scrollTop {
                    Self.scrollTo(target)
                    // The pointer hasn't moved, but the content under it has.
                    pointer += target - scrollTop
                }
            }
            try? await Task.sleep(for: .milliseconds(16))
        }
    }
}

/// over a hovered cover, plays the playlist from the start; on the playlist that's playing it pauses and resumes.
private struct CoverPlay: View {
    let row: PlaylistRef
    let shell: ShellModel
    @State private var hovering = false

    var body: some View {
        let current = shell.player.queueSource?.link == .playlist(row.id)
        let pause = current && shell.player.isPlaying
        let label = pause ? L("Pause {name}", ["name": row.name]) : L("Play {name}", ["name": row.name])
        if current || (shell.library.playlist(id: row.id)?.songCount ?? 0) > 0 {
            Button {
                if current { shell.player.togglePlayPause() }
                else if let playlist = shell.library.playlist(id: row.id) { shell.playPlaylist(playlist, shuffle: false) }
            } label: {
                ArtOverlay(shown: hovering) {
                    Image(systemName: pause ? "pause.fill" : "play.fill").font(.system(size: T.sidebarCoverGlyph))
                }
                .frame(width: T.sidebarCover, height: T.sidebarCover)
                .clipShape(RoundedRectangle(cornerRadius: T.sidebarCoverR, style: .continuous))
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .onHover { hovering = $0 }
            .help(label)
            .accessibilityLabel(label)
        }
    }
}

private struct SidebarRow<Icon: View>: View {
    let label: String
    let on: Bool
    var mark = false
    var download: AnyView? = nil
    @ViewBuilder let icon: Icon
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: T.px(12)) {
                icon
                Text(label).font(.navia(T.textMD, on ? T.sidebarActiveWeight : .regular)).foregroundStyle((on ? T.tEm : T.tNav).color).lineLimit(1).truncationMode(.tail).fullTextHelp(label)
                Spacer(minLength: 0)
                HStack(spacing: T.px(8)) {
                    if let download { download }
                    if mark {
                        Image(systemName: "person.2").font(.system(size: T.px(11))).foregroundStyle(T.tNav.color).accessibilityLabel(L("Public"))
                    }
                }
            }
            .padding(.horizontal, T.sidebarItemPadX)
            .frame(height: T.px(45))
            .background(RoundedRectangle(cornerRadius: T.rMD, style: .continuous).fill(on ? T.hover.color : hovering || HoverDebug.forced("Sidebar:" + label) ? T.row.color : .clear))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
    }
}
