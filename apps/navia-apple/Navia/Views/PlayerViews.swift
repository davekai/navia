import SwiftUI

/// the dock's left edge, a `dockGrabW` strip; double-click resets to the default width.
struct DockResizeHandle: NSViewRepresentable {
    let shell: ShellModel
    let windowWidth: CGFloat
    let sidebar: CGFloat?

    func makeNSView(context: Context) -> EdgeResizeView {
        let view = EdgeResizeView()
        view.toolTip = L("Drag to resize. Double-click to reset.")
        view.setAccessibilityLabel(L("Resize player dock"))
        return view
    }

    func updateNSView(_ view: EdgeResizeView, context: Context) {
        view.growsRight = false
        view.onDrag = { delta in shell.dockDragWidth = clamped(shell.dockWidthSaved + delta) }
        view.onEnd = {
            if let drag = shell.dockDragWidth { shell.dockWidthSaved = drag }
            shell.dockDragWidth = nil
        }
        view.onReset = { shell.dockWidthSaved = T.dockW }
    }

    private func clamped(_ tokens: CGFloat) -> CGFloat {
        let sidebar = sidebar.map { $0 + T.windowInset } ?? 0
        let room = (windowWidth - sidebar - T.windowInset - T.pageMinW) / T.scale
        return min(max(T.dockMinW, min(tokens, T.dockMaxW)), max(T.dockMinW, room))
    }
}

/// the sidebar's right edge, the dock edge's twin.
struct SidebarResizeHandle: NSViewRepresentable {
    let shell: ShellModel
    let windowWidth: CGFloat
    let docked: Bool

    func makeNSView(context: Context) -> EdgeResizeView {
        let view = EdgeResizeView()
        view.toolTip = L("Drag to resize. Double-click to reset.")
        view.setAccessibilityLabel(L("Resize sidebar"))
        return view
    }

    func updateNSView(_ view: EdgeResizeView, context: Context) {
        view.growsRight = true
        view.onDrag = { delta in
            let room = shell.sidebarRoomTokens(windowWidth: windowWidth, docked: docked)
            shell.sidebarDragWidth = min(max(T.sidebarWMin, shell.sidebarWidthSaved + delta), room)
        }
        view.onEnd = {
            if let drag = shell.sidebarDragWidth { shell.sidebarWidthSaved = drag }
            shell.sidebarDragWidth = nil
        }
        view.onReset = { shell.sidebarWidthSaved = T.sidebarW }
    }
}

/// AppKit, so the resize cursor and drag are exact and the strip takes only its own width.
/// `growsRight` is true when the panel widens as the pointer moves right (the sidebar).
final class EdgeResizeView: NSView {
    var growsRight = false
    var onDrag: (CGFloat) -> Void = { _ in }
    var onEnd: () -> Void = {}
    var onReset: () -> Void = {}
    private var startX: CGFloat?

    override var mouseDownCanMoveWindow: Bool { false }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override func resetCursorRects() { addCursorRect(bounds, cursor: .resizeLeftRight) }

    override func mouseDown(with event: NSEvent) {
        if event.clickCount == 2 { onReset(); return }
        startX = event.locationInWindow.x
    }

    override func mouseDragged(with event: NSEvent) {
        guard let startX else { return }
        let moved = event.locationInWindow.x - startX
        onDrag((growsRight ? moved : -moved) / T.scale)
    }

    override func mouseUp(with event: NSEvent) {
        if startX != nil { onEnd() }
        startX = nil
    }
}

struct PlayerDock: View {
    let shell: ShellModel
    let width: CGFloat
    /// The dock's width ignoring a drag in progress, so the blurred reflection only recomputes when a drag ends.
    let settledWidth: CGFloat
    let windowHeight: CGFloat
    @Environment(\.isSnapshot) private var isSnapshot
    @State private var artScrolled: CGFloat = 0
    private var player: any PlayerViewModel { shell.player }

    var body: some View {
        let tint = Tint(hex: player.current?.tintHex)
        let shape = RoundedRectangle(cornerRadius: T.rLG, style: .continuous)
        VStack(spacing: 0) {
            ZStack(alignment: .top) {
                bodyView(tint: tint)
                header
            }
            .frame(maxHeight: .infinity)
            footer(tint: tint)
        }
        .frame(width: width)
        .foregroundStyle(tint.playerFg.color)
        .environment(\.playerDim, tint.playerFgDim.color)
        .environment(\.hoverInk, tint.playerFg.color)
        .environment(\.springPress, true)
        .background(
            LinearGradient(stops: [.init(color: tint.playerVivid.color, location: 0.35),
                                   .init(color: tint.playerVivid.mixed(with: T.dockShade, amount: T.playerShadeMix).color, location: 1)],
                           startPoint: .top, endPoint: .bottom)
        )
        .clipShape(shape)
        .overlay(shape.strokeBorder(T.glassEdge.color, lineWidth: 1).allowsHitTesting(false))
        // Matches GPUI's dock shadow.
        .shadow(T.shadowDock)
    }

    private func bodyView(tint: Tint) -> some View {
        Swap(key: shell.shownPlayerView, kind: .view) {
            switch shell.shownPlayerView {
            case .artwork: artworkScroll(tint: tint)
            // covered by the full screen player, the dock's lyrics stop rather than run unseen.
            case .lyrics: if shell.expanded { Color.clear } else { LyricsView(shell: shell).padding(.top, T.px(76)) }
            case .queue: QueueListView(shell: shell).padding(.top, T.px(76))
            }
        }
    }

    private var header: some View {
        ZStack {
            LinearGradient(colors: [T.dockHeadShade.color, .clear], startPoint: .top, endPoint: .bottom)
                .frame(height: T.px(100))
                .frame(maxHeight: .infinity, alignment: .top)
                .allowsHitTesting(false)
            // Expand and the mini player on the left, the switcher centered regardless.
            ZStack {
                HStack {
                    FullScreenButton(shell: shell, size: T.px(18))
                    MiniPlayerButton(shell: shell, size: T.px(18))
                    Spacer()
                    IconButton(symbol: "rectangle.bottomthird.inset.filled", label: L("Move player to the bottom"), size: T.px(18)) { shell.playerMode = .bar }
                }
                ViewSwitcher(shell: shell)
            }
            .padding(.horizontal, T.px(12))
            .frame(height: T.px(76))
            .frame(maxHeight: .infinity, alignment: .top)
            .foregroundStyle(T.tOnArt.color)
        }
    }

    private func artworkScroll(tint: Tint) -> some View {
        let artHeight = min(T.px(502), windowHeight * 0.56 * T.scale) // 56% of the window height, at the interface scale.
        let kind: ArtworkKind = player.station == nil ? .album : .station
        let content = VStack(alignment: .leading, spacing: 0) {
            Swap(key: player.current?.id, kind: .song) {
            ZStack(alignment: .top) {
                ArtworkView(url: player.current?.artworkURL, kind: kind, alignment: .bottom)
                    .frame(width: settledWidth, height: T.px(240))
                    .scaleEffect(x: width / max(1, settledWidth), y: -1)
                    .frame(width: width)
                    .saturation(T.reflectSaturation)
                    .blur(radius: T.reflectBlur)
                    .opacity(T.reflectOpacity)
                    // The mask flips with the image, so it runs from the top: strongest at the cover, gone at the bottom edge.
                    .mask(LinearGradient(stops: [.init(color: .black, location: 0), .init(color: .black, location: 0.2), .init(color: .clear, location: 1)], startPoint: .top, endPoint: .bottom))
                    .offset(y: artHeight - T.px(95))
                    .allowsHitTesting(false)
                ArtworkView(url: player.current?.artworkURL, kind: kind, alignment: .top, animates: !shell.expanded)
                    .frame(height: artHeight)
                    .mask(LinearGradient(stops: [.init(color: .black, location: 0.8), .init(color: .clear, location: 1)], startPoint: .top, endPoint: .bottom))
            }
            }
            .frame(height: artHeight, alignment: .top)

            if !player.featuredArtists.isEmpty { featured.padding(.top, T.px(-8)) }
        }
        // On short windows Featured Artists scrolls, fading out above the title and at the top once scrolled.
        let topFade = min(1, max(0, artScrolled / T.px(28)))
        return Group {
            if isSnapshot { content.frame(minHeight: 0, maxHeight: .infinity, alignment: .top).clipped() } else {
                ScrollView(.vertical) { content }
                    .scrollIndicators(.never)
                    .onScrollGeometryChange(for: CGFloat.self, of: { $0.contentOffset.y + $0.contentInsets.top }) { _, y in artScrolled = y }
            }
        }
        .mask(
            VStack(spacing: 0) {
                LinearGradient(colors: [.black.opacity(1 - topFade), .black], startPoint: .top, endPoint: .bottom).frame(height: T.px(28))
                Color.black
                LinearGradient(colors: [.black, .clear], startPoint: .top, endPoint: .bottom).frame(height: T.px(28))
            }
        )
    }

    private var featured: some View {
        VStack(alignment: .leading, spacing: T.px(14)) {
            Text(L("Featured Artists"))
                .font(.navia(T.textSection, .semibold))
                .tracking(-0.012 * T.textSection)
                .foregroundStyle(T.tOnArt.color)
            HScrollRow(leading: T.px(32), trailing: T.px(32), spacing: T.px(20), arrowTop: T.px(40), edgeFade: T.px(40)) {
                ForEach(player.featuredArtists) { artist in
                    FeaturedArtistCell(artist: artist) {
                        if let id = artist.artistID { shell.go(.artist(id)) }
                    }
                }
            }
            .padding(.bottom, T.px(4))
            .padding(.horizontal, T.px(-32))
        }
        .padding(.horizontal, T.px(32))
        .padding(.bottom, T.px(44))
    }

    private func footer(tint: Tint) -> some View {
        PlayerControls(shell: shell, skipSize: T.px(32), playSize: T.px(44), swapsSong: true)
            .padding(.top, T.px(12))
            .padding(.horizontal, T.px(32))
            .padding(.bottom, T.px(28))
    }
}

private struct FeaturedArtistCell: View {
    let artist: FeaturedArtist
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            VStack(spacing: T.px(8)) {
                ArtworkView(url: artist.photoURL, kind: .artist, pixels: 300)
                    .frame(width: T.px(116), height: T.px(116))
                    .clipShape(Circle())
                Text(artist.name)
                    .font(.navia(T.textMD))
                    .foregroundStyle(T.tOnArt.color)
                    .multilineTextAlignment(.center)
                    .lineLimit(2)
                    .frame(width: T.px(116))
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(artist.artistID == nil)
    }
}

struct PlayerControls: View {
    let shell: ShellModel
    let skipSize: CGFloat
    let playSize: CGFloat
    /// the title cross-fades on a song change (the side player); full screen it just changes.
    var swapsSong = false
    @Environment(\.playerDim) private var dim
    private var player: any PlayerViewModel { shell.player }

    var body: some View {
        VStack(spacing: 0) {
            if PlayerChipSlot.shows(player) {
                PlayerChipSlot(player: player).padding(.bottom, T.px(12))
            }
            HStack(spacing: 0) {
                Swap(key: swapsSong ? player.current?.id : nil, kind: .song, alignment: .leading) {
                VStack(alignment: .leading, spacing: T.px(2)) {
                    // The title links to its album and each artist to theirs.
                    if let song = player.current {
                        MarqueeText {
                            HStack(spacing: T.textLG * 0.27) {
                                LinkedText(pieces: [LinkPiece.title(song)], shell: shell).font(.navia(T.textLG, .medium)).foregroundStyle(T.tOnArt.color)
                                if song.isExplicit { ExplicitMark(size: T.textLG) }
                            }
                        }
                        MarqueeText { LinkedText(pieces: LinkPiece.artist(song), shell: shell).font(.navia(T.textMD)).playerDim(dim) }
                        OutputBadge(shell: shell).playerDim(dim).padding(.top, T.px(2))
                    }
                }
                }
                .lineLimit(1)
                .rightClickMenu { playerMenu(shell) }
                Spacer(minLength: T.px(8))
                FavoriteButton(player: player)
                TrackMenuButton(shell: shell)
            }
            .padding(.bottom, T.playerMetaGap)

            ScrubberStacked(shell: shell)

            HStack {
                ShuffleButton(shell: shell, size: T.px(24))
                Spacer()
                SkipButton(symbol: "backward.fill", size: skipSize, label: L("Previous"), disabled: player.station != nil) { player.previous() }
                Spacer()
                PlayPauseButton(player: player, size: playSize)
                Spacer()
                SkipButton(symbol: "forward.fill", size: skipSize, label: L("Next"), disabled: player.station != nil) { player.next() }
                Spacer()
                RepeatButton(shell: shell, size: T.px(24))
            }
            .padding(.top, T.playerTransportGap)
            .padding(.horizontal, T.px(-8))

            HStack(spacing: T.px(6)) {
                VolumeRow(player: player)
                OutputButton(shell: shell, size: T.px(17)).playerDim(dim)
            }
            .padding(.top, T.playerVolumeGap)
            .padding(.horizontal, T.px(-8))
        }
    }
}

struct ViewSwitcher: View {
    let shell: ShellModel
    private var hasLyrics: Bool { shell.player.mayHaveLyrics && shell.player.station == nil }

    var body: some View {
        HStack(spacing: 0) {
            tab("quote.bubble", label: L("Lyrics"), view: .lyrics, disabled: !hasLyrics)
            tab("music.note", label: L("Artwork"), view: .artwork, disabled: false)
            tab("list.bullet", label: L("Queue"), view: .queue, disabled: false)
        }
        .padding(T.px(4))
        .plainGlass(Capsule(), flat: T.glassOn)
    }

    private func tab(_ symbol: String, label: String, view: PlayerView, disabled: Bool) -> some View {
        let on = shell.shownPlayerView == view
        return Button { if !on { shell.playerView = view } } label: {
            Image(systemName: symbol)
                .font(.system(size: T.px(18)))
                .symbolVariant(on && view == .artwork ? .fill : .none)
                // The selected segment's icon is pure white. The others keep the player's ink.
                .foregroundStyle(on ? AnyShapeStyle(T.tEm.color) : AnyShapeStyle(.foreground))
                .frame(width: T.px(50), height: T.px(40))
                .background(Capsule().fill(on ? T.switcherOn.color : .clear))
                .animation(T.toggle, value: on)
                .contentShape(Capsule())
                .opacity(disabled ? 0.3 : on ? 1 : T.switcherRest)
        }
        .buttonStyle(HoverBrightenStyle(selected: on, key: label))
        .modifier(QueueTabDrop(view: view, shell: shell))
        .disabled(disabled)
        .help(disabled ? L("No lyrics for this song") : label)
        .accessibilityLabel(label)
    }
}

/// The Queue tab takes songs dropped on it. The other tabs don't.
private struct QueueTabDrop: ViewModifier {
    let view: PlayerView
    let shell: ShellModel
    func body(content: Content) -> some View {
        if view == .queue {
            content.songDropTarget(.queueEnd("tab"), shell: shell)
        } else {
            content
        }
    }
}

private struct ScrubberStacked: View {
    let shell: ShellModel
    private var player: any PlayerViewModel { shell.player }
    /// In the player the times take the dimmer ink. Elsewhere they're the foreground at 78%.
    @Environment(\.playerDim) private var dim
    var body: some View {
        #if DEBUG
        let _ = VisualStats.barBodies += 1
        #endif
        VStack(spacing: T.px(4)) {
            if player.station != nil {
                Capsule().fill(.foreground).opacity(0.3).frame(height: 4).padding(.vertical, T.px(5))
                HStack {
                    Text(L("Live")).fontWeight(.semibold)
                    PrivateBadge(player: player)
                    Spacer()
                    Text(clock(player.position))
                }
                .font(.navia(T.textSM).monospacedDigit())
                .playerDim(dim)
                .opacity(dim == nil ? 0.78 : 1)
            } else {
                TrackSlider(value: player.position, total: Double(player.current?.duration ?? 0), rest: T.playerTrack) { player.seek(to: $0) }
                // Between the times: Private mode's mark, Waiting for server and the quality badge.
                HStack(spacing: 0) {
                    ScrubTime(shell: shell, side: .elapsed)
                    Spacer(minLength: T.px(8))
                    HStack(spacing: T.px(10)) {
                        PrivateBadge(player: player)
                        WaitingBadge(player: player)
                        QualityBadge(shell: shell)
                    }
                    Spacer(minLength: T.px(8))
                    ScrubTime(shell: shell, side: .end)
                }
                .font(.navia(T.textSM).monospacedDigit())
                .playerDim(dim)
                .opacity(dim == nil ? 0.78 : 1)
            }
        }
    }
}

/// A time beside a scrubber: elapsed, or at the end the length or what's left (`timeShowsRemaining`); a click switches it.
struct ScrubTime: View {
    enum Side { case elapsed, end }
    let shell: ShellModel
    let side: Side

    var body: some View {
        let remaining = shell.timeShowsRemaining
        Button { shell.timeShowsRemaining.toggle() } label: {
            Text(side == .elapsed ? clock(shell.player.position) : Self.endText(shell)).contentShape(Rectangle())
        }
        .buttonStyle(PressStyle(pill: true, key: side == .elapsed ? "time-elapsed" : "time-end"))
        .help(remaining ? L("Show song length") : L("Show remaining time"))
    }

    static func endText(_ shell: ShellModel) -> String {
        let total = Double(shell.player.current?.duration ?? 0)
        return shell.timeShowsRemaining ? "\u{2212}" + clock(max(0, total - shell.player.position)) : clock(total)
    }
}

struct PrivateBadge: View {
    let player: any PlayerViewModel
    var body: some View {
        if player.privateMode {
            HStack(spacing: T.px(4)) {
                Image(systemName: "eye.slash").font(.system(size: T.px(13)))
                Text(L("Private"))
            }
            .font(.navia(T.textXS))
            .opacity(0.8)
            .padding(.leading, T.px(8))
            .help(L("Private mode is on. Plays aren't counted or scrobbled."))
        }
    }
}

/// Beside the time while playback has run out of audio and is holding its place for the server.
struct WaitingBadge: View {
    let player: any PlayerViewModel
    var body: some View {
        if player.isWaitingForServer {
            Text(L("Waiting for server"))
                .font(.navia(T.textXS))
                .opacity(0.8)
                .padding(.leading, T.px(8))
                .help(L("The server can't be reached. Playback picks up when it's back."))
        }
    }
}

struct VolumeRow: View {
    let player: any PlayerViewModel
    @Environment(\.playerDim) private var dim
    var body: some View {
        HStack(spacing: T.px(6)) {
            // The icon takes the dimmer step; the level keeps the bright one.
            IconButton(symbol: volumeSymbol(player), label: player.isMuted ? L("Unmute") : L("Mute"), size: T.px(16), level: volumeLevel(player)) { player.isMuted.toggle() }.playerDim(dim)
            // on AirPlay the slider drives the receiver's own volume.
            let receiver = AirPlayModel.shared.active
            TrackSlider(value: player.isMuted ? 0 : player.volume, total: 100, rest: T.playerTrack, onWheel: { player.stepVolume($0) }) { player.setVolumeFromSlider($0) }
                .accessibilityLabel(receiver.map { L("Volume on {device}", ["device": $0.name]) } ?? L("Volume"))
                .help(receiver.map { L("Volume on {device}", ["device": $0.name]) } ?? "")
        }
    }
}

private let volumeWheelStep: Double = 5

extension PlayerViewModel {
    func setVolumeFromSlider(_ value: Double) {
        volume = value
        if value > 0 { isMuted = false }
    }

    /// One wheel step is 5 of 100.
    func stepVolume(_ steps: Int) { setVolumeFromSlider(min(100, max(0, volume + Double(steps) * volumeWheelStep))) }
}

private func volumeSymbol(_ player: any PlayerViewModel) -> String {
    player.isMuted || player.volume == 0 ? "speaker.slash.fill" : "speaker.wave.3.fill"
}

/// the waves light up one by one as the volume rises.
private func volumeLevel(_ player: any PlayerViewModel) -> Double { player.isMuted ? 0 : player.volume / 100 }

private struct FavoriteButton: View {
    let player: any PlayerViewModel
    var body: some View {
        IconButton(symbol: "heart", label: L("Favorite"), active: player.current?.isFavorite ?? false, disabled: player.station != nil, pop: true) { player.toggleFavorite() }
    }
}

private struct TrackMenuButton: View {
    let shell: ShellModel
    var body: some View {
        MenuIconButton(symbol: "ellipsis", label: L("More")) { playerMenu(shell) }
    }
}

/// The player's menu, from More or a right-click on the song: the song's actions, then the player's.
@MainActor func playerMenu(_ shell: ShellModel) -> [MenuEntry] {
    let player = shell.player
    var entries: [MenuEntry]
    if player.station != nil {
        entries = [MenuEntry(title: L("Stop Station")) { player.stopStation() }]
    } else if let song = player.current {
        entries = SongMenu.single(song, shell: shell)
    } else {
        entries = []
    }
    entries.append(.divider)
    entries.append(MenuEntry(title: L("Mini Player"), shortcut: MiniPlayer.shortcutKey, shortcutModifiers: MiniPlayer.shortcutModifiers) { MiniPlayer.shared.open(shell: shell) })
    entries.append(MenuEntry(title: L("Private Mode"), checked: player.privateMode) { player.privateMode.toggle() })
    return entries
}

private let noStationModes = "Stations can't be shuffled or repeated"

/// The player's plain icon button. Dim when off, the accent with a dot under the glyph when on. `badge` is repeat
/// one's "1". `help` replaces the label as the tooltip, to say why it's disabled.
struct ModeButton: View {
    let symbol: String
    let label: String
    let size: CGFloat
    let on: Bool
    var badge = false
    let accent: RGBA
    let onAccent: RGBA
    let disabled: Bool
    var help: String? = nil
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        let box = size + T.px(12)
        Button(action: action) {
            ZStack {
                Image(systemName: symbol).font(.system(size: size * 0.78, weight: .bold))
                if on {
                    Circle().frame(width: T.px(4), height: T.px(4)).frame(maxHeight: .infinity, alignment: .bottom)
                }
                if badge {
                    Text("1")
                        .font(.system(size: T.px(9.5), weight: .bold).monospacedDigit())
                        .foregroundStyle(onAccent.color)
                        .frame(minWidth: T.px(13), minHeight: T.px(13))
                        .padding(.horizontal, T.px(2))
                        .background(Capsule().fill(accent.color))
                        .fixedSize()
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
                        .padding(.top, T.px(1))
                }
            }
            .frame(width: box, height: box)
            .foregroundStyle(on ? AnyShapeStyle(accent.color) : AnyShapeStyle(.foreground))
            .animation(T.toggle, value: on)
            .animation(T.toggle, value: badge)
            .contentShape(Rectangle())
        }
        .buttonStyle(HoverBrightenStyle(key: label, glyph: size * 0.78))
        .opacity(disabled ? T.modeDisabled : on || hovering ? 1 : T.modeRest)
        .animation(T.toggle, value: on)
        .disabled(disabled)
        .onHover { hovering = $0 }
        .help(help ?? label)
        .accessibilityLabel(label)
        .accessibilityAddTraits(on ? .isSelected : [])
    }
}

struct ShuffleButton: View {
    let shell: ShellModel
    let size: CGFloat
    var label = L("Shuffle")
    var body: some View {
        let player = shell.player
        ModeButton(symbol: "shuffle", label: label, size: size, on: player.shuffle, accent: shell.theme.accentSoft, onAccent: shell.theme.onAccent,
                   disabled: player.station != nil, help: player.station != nil ? L(noStationModes) : nil) { player.shuffle.toggle() }
    }
}

struct RepeatButton: View {
    let shell: ShellModel
    let size: CGFloat
    var body: some View {
        let player = shell.player
        ModeButton(symbol: "repeat", label: player.repeatMode.title, size: size, on: player.repeatMode != .off, badge: player.repeatMode == .one,
                   accent: shell.theme.accentSoft, onAccent: shell.theme.onAccent, disabled: player.station != nil,
                   help: player.station != nil ? L(noStationModes) : nil) { player.repeatMode = player.repeatMode.next }
    }
}

/// Previous or next. `box` sets the button's square when it differs from the glyph plus margin (the mini player).
struct SkipButton: View {
    let symbol: String
    let size: CGFloat
    let label: String
    var disabled = false
    var box: CGFloat? = nil
    let action: () -> Void
    var body: some View {
        Button(action: action) {
            Image(systemName: symbol).font(.system(size: size * 0.8)).frame(width: box ?? size + 8, height: box ?? size + 8).contentShape(Rectangle())
        }
        .buttonStyle(HoverGrowStyle(key: label, glyph: size * 0.8))
        .disabled(disabled)
        .opacity(disabled ? 0.35 : 1)
        .help(disabled ? L("Stations can't be skipped") : label)
        .accessibilityLabel(label)
    }
}

struct PlayPauseButton: View {
    let player: any PlayerViewModel
    let size: CGFloat
    var box: CGFloat? = nil
    var body: some View {
        Button { player.togglePlayPause() } label: {
            Image(systemName: player.isPlaying ? "pause.fill" : "play.fill")
                .font(.system(size: size * 0.8))
                .contentTransition(.opacity)
                .animation(T.easeOut(T.pressSwap), value: player.isPlaying)
                .frame(width: box ?? size + 6, height: box ?? size + 6)
                .contentShape(Rectangle())
        }
        .buttonStyle(HoverGrowStyle(key: "PlayPause", glyph: size * 0.8))
        .help(player.isPlaying ? L("Pause") : L("Play"))
        .accessibilityLabel(player.isPlaying ? L("Pause") : L("Play"))
    }
}

struct PlayerBar: View {
    let shell: ShellModel
    let canDock: Bool
    /// how far it's in as it slides up from below the window, 0 to 1.
    var shown: CGFloat = 1
    @State private var volumeHover = BarVolumeHover()
    @State private var barScrubHover = false
    @State private var barScrubWait: Task<Void, Never>?
    @State private var barArtHover = false
    private var player: any PlayerViewModel { shell.player }

    var body: some View {
        let tint = Tint(hex: player.current?.tintHex)
        ZStack(alignment: .bottom) {
            // fractions apply to both axes, so the frame is 1 / 0.52 taller for an ellipse 52% wide and full height.
            EllipticalGradient(colors: [tint.barGlow.opacity(T.barGlowStrength).color, .clear], center: .bottom, startRadiusFraction: 0, endRadiusFraction: 0.52)
                .frame(height: T.px(150) / 0.52)
                .frame(height: T.px(150), alignment: .bottom)
                .frame(maxWidth: .infinity)
                .allowsHitTesting(false)
                .opacity(shown)

            GeometryReader { geo in
                let fit = BarFit(width: geo.size.width / T.scale, canDock: canDock)
                bar(drop: !fit.modes ? 3 : !fit.optional ? 2 : !fit.extras ? 1 : 0, compact: !fit.side)
                    // Centered above the play controls, outside the bar's glass so it stays clickable.
                    .overlayPreferenceValue(BarTransportAnchor.self) { anchor in
                        GeometryReader { geo in
                            if let anchor {
                                PlayerChipSlot(player: player, overBar: true)
                                    .fixedSize()
                                    .frame(maxWidth: T.chipAboveMaxW)
                                    // A zero-size frame the chip hangs above, centered on it.
                                    .frame(width: 0, height: 0, alignment: .bottom)
                                    .position(x: geo[anchor].midX, y: -T.chipAboveGap)
                            }
                        }
                    }
            }
            .frame(height: T.barH)
            .frame(maxWidth: T.barW)
            .padding(.horizontal, T.px(14))
            .padding(.bottom, T.windowInset)
            .offset(y: (T.barH + T.windowInset) * (1 - shown))
        }
        .environment(\.activeFills, true)
        .environment(\.springPress, true)
        // the first click on an inactive window presses the control at once.
        .allowsWindowActivationEvents(true)
        .overlayPreferenceValue(BarVolumeAnchor.self) { anchor in
            GeometryReader { geo in
                if let anchor, volumeHover.open {
                    let frame = geo[anchor]
                    BarVolumePanel(player: player, hover: volumeHover)
                        .offset(x: frame.midX - BarVolumePanel.width / 2, y: frame.minY - BarVolumePanel.height)
                }
            }
        }
    }

    private func openExpanded(_ view: PlayerView? = nil) {
        if let view { shell.playerView = view }
        shell.setExpanded(true)
    }

    /// Lyrics and Queue open the docked player on that view. A window too narrow to dock opens them full screen.
    private func openSide(_ view: PlayerView) {
        guard canDock else { openExpanded(view); return }
        shell.playerView = view
        shell.playerMode = .dock
    }

    /// Narrow bars drop extras in order: 1 Output and More, 2 heart and volume, 3 shuffle and repeat.
    private func bar(drop: Int, compact: Bool) -> some View {
        // the bar is neutral, never the cover tint.
        let fg = T.tOnArt.color, dim = T.barInk.color
        return CentredRow(gap: T.px(BarFit.gap)) {
            // over the progress line it rises and the times show over the rows, blurred behind them.
            let timesShown = player.station == nil && barScrubHover
            // the line's slot is taller than the line, so it overlaps upward by its empty half.
            VStack(alignment: .leading, spacing: T.barLineGap - (T.scrubSlot - T.barScrubRest) / 2) {
                ZStack(alignment: .bottomLeading) {
                    HStack(spacing: T.px(BarFit.gap)) {
                        Button { openExpanded() } label: {
                            Swap(key: player.current?.id, kind: .song) {
                                ArtworkView(url: player.current?.artworkURL, kind: player.station == nil ? .album : .station, pixels: 120)
                            }
                                .frame(width: T.barArt, height: T.barArt)
                                .overlay {
                                    ArtOverlay(shown: barArtHover) {
                                        Image(systemName: "arrow.up.left.and.arrow.down.right").font(.system(size: T.barArtExpand, weight: .bold))
                                    }
                                }
                                .clipShape(RoundedRectangle(cornerRadius: T.rSM, style: .continuous))
                                .scaleEffect(barArtHover ? T.barArtGrow : 1)
                                .animation(T.hover(barArtHover), value: barArtHover)
                                .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .onHover { barArtHover = $0 }
                        .help("\(L("Expand player")) (⇧⌘F)")
                        .accessibilityLabel(L("Expand player"))
                        Swap(key: player.current?.id, kind: .song, alignment: .leading) {
                        VStack(alignment: .leading, spacing: T.px(1)) {
                            HStack(spacing: T.px(10)) {
                                MarqueeText { HStack(spacing: 0) {
                                    if let song = player.current {
                                        LinkedText(pieces: [LinkPiece.title(song)], shell: shell).font(.navia(T.textMD, .medium)).foregroundStyle(T.barTitle.color).layoutPriority(1)
                                        if song.isExplicit { ExplicitMark(size: T.textMD).padding(.leading, T.textMD * 0.27) }
                                    }
                                } }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                QualityBadge(shell: shell, bar: true, glyphOnly: drop >= 1).foregroundStyle(dim)
                                PrivateBadge(player: player).foregroundStyle(dim)
                                WaitingBadge(player: player).foregroundStyle(dim)
                                OutputBadge(shell: shell, compact: true, glyphOnly: drop >= 1).foregroundStyle(dim)
                            }
                            if let song = player.current, !song.artist.isEmpty {
                                MarqueeText { LinkedText(pieces: LinkPiece.artist(song), shell: shell) }
                                .font(.navia(T.textSM))
                                .foregroundStyle(dim)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            }
                        }
                        }
                        .lineLimit(1)
                        // the artwork sets the block's height, so it and the line end up equally far from the bar's edges.
                        .frame(height: T.barArt)
                        .rightClickMenu { playerMenu(shell) }
                    }
                    .blur(radius: timesShown ? T.barRowsBlur : 0)
                    .opacity(timesShown ? T.barRowsDim : 1)
                    .allowsHitTesting(!timesShown)
                    if player.station == nil {
                        HStack(spacing: T.px(12)) {
                            ScrubTime(shell: shell, side: .elapsed).lineLimit(1).fixedSize()
                            Spacer(minLength: 0)
                            QualityBadge(shell: shell, bar: true)
                            Spacer(minLength: 0)
                            ScrubTime(shell: shell, side: .end).lineLimit(1).fixedSize()
                        }
                        .font(.navia(T.textSM).monospacedDigit())
                        .foregroundStyle(dim)
                        .opacity(timesShown ? 1 : 0)
                        .offset(y: timesShown ? -T.barScrubRise : 0)
                        .allowsHitTesting(timesShown)
                    }
                }
                .animation(T.reveal(timesShown), value: timesShown)
                if player.station != nil {
                    HStack(spacing: T.px(12)) {
                        Text(L("Live")).fontWeight(.semibold).lineLimit(1).fixedSize()
                        Capsule().fill(.foreground).opacity(0.3).frame(height: T.barScrubRest)
                    }
                    .font(.navia(T.textSM))
                    .foregroundStyle(dim)
                } else {
                    // hover is tracked on the unmoved slot, so the rise can't carry the line out from under the pointer.
                    ZStack {
                        TrackSlider(value: player.position, total: Double(player.current?.duration ?? 0), rest: T.barTrack, grows: true, raised: timesShown) { player.seek(to: $0) }
                            .foregroundStyle(fg)
                            .offset(y: timesShown ? -T.barScrubRise : 0)
                            .animation(T.reveal(timesShown), value: timesShown)
                    }
                    .padding(.top, T.barScrubReach)
                    .contentShape(Rectangle())
                    .onHover { over in
                        barScrubWait?.cancel()
                        // A short wait, so passing over the line on the way to the artwork doesn't raise it.
                        if over { barScrubWait = Task { try? await Task.sleep(for: .seconds(T.barScrubDelay)); if !Task.isCancelled { barScrubHover = true } } }
                    }
                    .padding(.top, -T.barScrubReach)
                }
            }
            .padding(.top, T.barArtInset)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            // once raised, the line stays up while the pointer is anywhere over the block, so the times can be reached.
            .onHover { if !$0 { barScrubWait?.cancel(); barScrubHover = false } }
            .padding(.leading, T.barArtInset)

            HStack(spacing: T.px(6)) {
                if drop < 3 { ShuffleButton(shell: shell, size: T.px(20)) }
                SkipButton(symbol: "backward.fill", size: T.px(23), label: L("Previous"), disabled: player.station != nil) { player.previous() }
                PlayPauseButton(player: player, size: T.px(32))
                SkipButton(symbol: "forward.fill", size: T.px(23), label: L("Next"), disabled: player.station != nil) { player.next() }
                if drop < 3 { RepeatButton(shell: shell, size: T.px(20)) }
            }
            .foregroundStyle(fg)
            .anchorPreference(key: BarTransportAnchor.self, value: .bounds) { $0 }

            HStack(spacing: 0) {
                if !compact {
                    if drop < 1 { TrackMenuButton(shell: shell) }
                    if drop < 2 {
                        FavoriteButton(player: player)
                        Spacer().frame(width: T.px(16))
                    }
                    if drop < 1 { OutputButton(shell: shell, size: T.px(17)) }
                    if drop < 2 {
                        BarVolumeButton(player: player, hover: volumeHover)
                        Spacer().frame(width: T.px(16))
                    }
                    let hasLyrics = player.mayHaveLyrics && player.station == nil
                    IconButton(symbol: "quote.bubble", label: hasLyrics ? L("Lyrics") : L("No lyrics for this song"), disabled: !hasLyrics) { openSide(.lyrics) }
                    IconButton(symbol: "list.bullet", label: L("Queue")) { openSide(.queue) }
                        .songDropTarget(.queueEnd("bar"), shell: shell)
                    MiniPlayerButton(shell: shell)
                    if canDock {
                        IconButton(symbol: "rectangle.righthalf.inset.filled", label: L("Dock on the right")) { shell.playerMode = .dock }
                    }
                }
            }
            // The side buttons take the bar's ink, like the transport.
            .foregroundStyle(fg)
            .padding(.trailing, T.px(BarFit.padTrailing))
            .frame(maxWidth: .infinity, alignment: .trailing)
        }
        .frame(maxHeight: .infinity)
        .plainGlass(RoundedRectangle(cornerRadius: T.px(22), style: .continuous), flat: T.floatGlass)
        .shadow(T.shadowFloat)
    }
}

/// What a bar `width` (design px) wide shows. The play controls sit at the exact center, so the right group must fit
/// in half of what's left. Extras drop in order (Output and More, heart and volume, shuffle and repeat, then the
/// whole group) at fixed widths, or sooner if they don't fit. Matches GPUI's `bar_fit`.
struct BarFit {
    static let padTrailing: CGFloat = 14, gap: CGFloat = 18
    private static let icon: CGFloat = 38, spacer: CGFloat = 16, skip: CGFloat = 23, play: CGFloat = 32, transportGap: CGFloat = 6
    private static let mode: CGFloat = 20 + 12
    let extras: Bool, optional: Bool, modes: Bool, side: Bool

    init(width: CGFloat, canDock: Bool) {
        let base = (canDock ? 4 : 3) * Self.icon
        func right(extras: Bool, optional: Bool) -> CGFloat {
            base + (optional ? 2 * Self.icon + 2 * Self.spacer : 0) + (extras ? 2 * Self.icon : 0) + Self.padTrailing
        }
        func transport(_ modes: Bool) -> CGFloat {
            2 * Self.skip + Self.play + 2 * Self.transportGap + (modes ? 2 * (Self.mode + Self.transportGap) : 0)
        }
        func fits(_ right: CGFloat, _ modes: Bool) -> Bool { right <= (width - transport(modes) - 2 * Self.gap) / 2 }
        modes = width > 600 && fits(right(extras: false, optional: false), true)
        extras = width > 880 && fits(right(extras: true, optional: true), modes)
        optional = width > 760 && fits(right(extras: false, optional: true), modes)
        side = width > 430 && fits(right(extras: false, optional: false), modes)
    }
}

/// Three views in a row: the middle one at the exact center, the outer two given equal widths `gap` from it.
private struct CentredRow: Layout {
    let gap: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let height = proposal.height ?? subviews.map { $0.sizeThatFits(.unspecified).height }.max() ?? 0
        return CGSize(width: proposal.width ?? 0, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        guard subviews.count == 3 else { return }
        let middle = subviews[1].sizeThatFits(.unspecified)
        let side = ProposedViewSize(width: max(0, (bounds.width - middle.width) / 2 - gap), height: bounds.height)
        subviews[0].place(at: CGPoint(x: bounds.minX, y: bounds.midY), anchor: .leading, proposal: side)
        subviews[1].place(at: CGPoint(x: bounds.midX, y: bounds.midY), anchor: .center, proposal: ProposedViewSize(middle))
        subviews[2].place(at: CGPoint(x: bounds.maxX, y: bounds.midY), anchor: .trailing, proposal: side)
    }
}

/// The output picker: this Mac's devices, then AirPlay receivers. Devices are read on hover, so newly plugged
/// headphones show up. While a receiver is the output the glyph is AirPlay's, in the accent once it plays.
private struct OutputButton: View {
    let shell: ShellModel
    let size: CGFloat

    var body: some View {
        let receiver = shell.airplay.active
        let playing = receiver?.state == .playing
        MenuIconButton(symbol: receiver == nil ? "laptopcomputer" : AirPlayGlyph.symbol,
                       label: receiver.map { L("Output: {device}", ["device": $0.name]) } ?? L("Output"),
                       size: size, tint: playing ? shell.theme.accent.color : nil, live: true,
                       opened: { shell.airplay.menuOpened() }) {
            shell.outputMenu()
        }
            .task { shell.loadOutputs() }
            .onHover { if $0 { shell.loadOutputs(refresh: true) } }
    }
}

/// the bar's speaker and its panel; `PlayerBar` draws the panel outside the bar's glass, which clips hit testing.
@MainActor @Observable private final class BarVolumeHover {
    private(set) var open = false
    @ObservationIgnored private var closing: Task<Void, Never>?

    /// Stays open briefly after the pointer leaves, so it can travel between the speaker and the panel.
    func hover(_ over: Bool) {
        closing?.cancel()
        closing = nil
        if over { open = true; return }
        closing = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(180))
            guard !Task.isCancelled else { return }
            self?.open = false
        }
    }
}

/// Where the bar's play controls are, so the player chip can sit centered above them.
private struct BarTransportAnchor: PreferenceKey {
    static var defaultValue: Anchor<CGRect>? { nil }
    static func reduce(value: inout Anchor<CGRect>?, nextValue: () -> Anchor<CGRect>?) { value = value ?? nextValue() }
}

private struct BarVolumeAnchor: PreferenceKey {
    static var defaultValue: Anchor<CGRect>? { nil }
    static func reduce(value: inout Anchor<CGRect>?, nextValue: () -> Anchor<CGRect>?) { value = value ?? nextValue() }
}

/// The bar's speaker. Hover opens the slider above it, and a click mutes.
private struct BarVolumeButton: View {
    let player: any PlayerViewModel
    let hover: BarVolumeHover

    var body: some View {
        IconButton(symbol: volumeSymbol(player), label: player.isMuted ? L("Unmute") : L("Mute"), level: volumeLevel(player)) { player.isMuted.toggle() }
            .anchorPreference(key: BarVolumeAnchor.self, value: .bounds) { $0 }
            .onHover(perform: hover.hover)
    }
}

/// The slider panel above the speaker, including the gap down to it, so the pointer can cross without closing it.
private struct BarVolumePanel: View {
    let player: any PlayerViewModel
    let hover: BarVolumeHover
    static var width: CGFloat { T.px(170) }
    static var height: CGFloat { T.px(38) + gap }
    private static var gap: CGFloat { T.px(14) }

    var body: some View {
        TrackSlider(value: player.isMuted ? 0 : player.volume, total: 100, onWheel: { player.stepVolume($0) }) { player.setVolumeFromSlider($0) }
            .foregroundStyle(T.barInk.color)
            .frame(width: T.px(138))
            .padding(.horizontal, T.px(16))
            .padding(.vertical, T.px(12))
            .floatPanel()
            .padding(.bottom, Self.gap)
            .frame(width: Self.width, height: Self.height)
            .contentShape(Rectangle())
            .onHover(perform: hover.hover)
    }
}

private extension View {
    /// Applies the player's dimmer ink when there is one.
    @ViewBuilder func playerDim(_ dim: Color?) -> some View {
        if let dim { foregroundStyle(dim) } else { self }
    }
}

/// the full screen player's button (⇧⌘F): its glyph and label say whether it enters or leaves.
struct FullScreenButton: View {
    let shell: ShellModel
    var size: CGFloat = T.px(17)
    var body: some View {
        let on = shell.expanded
        IconButton(symbol: on ? "arrow.down.right.and.arrow.up.left" : "arrow.up.left.and.arrow.down.right",
                   label: on ? L("Collapse player") : L("Expand player"), size: size,
                   help: "\(on ? L("Collapse player") : L("Expand player")) (⇧⌘F)") { shell.setExpanded(!on) }
            .contentTransition(.symbolEffect(.replace))
    }
}

struct MiniPlayerButton: View {
    let shell: ShellModel
    var size: CGFloat = T.px(17)
    var body: some View {
        IconButton(symbol: MiniPlayer.symbol, label: L("Mini Player"), size: size, help: "\(L("Mini Player")) (\(MiniPlayer.shortcutLabel))") {
            MiniPlayer.shared.open(shell: shell)
        }
    }
}
