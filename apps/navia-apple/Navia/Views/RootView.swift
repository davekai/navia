import AppKit
import SwiftUI

struct RootView: View {
    let shell: ShellModel
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        GeometryReader { geo in
            let width = geo.size.width
            let sidebarFits = width >= T.sidebarMinWindow
            let canDock = width >= T.dockMinWindow
            let sidebarOpen = sidebarFits ? shell.sidebarPreferred : shell.drawerOpen
            let inlineSidebar = sidebarOpen && sidebarFits
            let mode: PlayerMode = shell.playerMode == .dock && !canDock ? .bar : shell.playerMode
            let hasPlayer = shell.player.current != nil
            let docked = hasPlayer && mode == .dock
            // by mode alone, so a player appearing (a session restored at launch) is just there.
            let target = PanelShown(sidebar: inlineSidebar ? 1 : 0, dock: mode == .dock ? 1 : 0, bar: mode == .bar ? 1 : 0)
            PanelStage(target: target, live: shell.windowShown) { shown, staged, warming in
                let sidebarIn = sidebarFits && (shown.sidebar > 0 || staged.sidebar > 0)
                let dockIn = hasPlayer && (shown.dock > 0 || staged.dock > 0)
                let barIn = hasPlayer && (shown.bar > 0 || staged.bar > 0)
                let sidebarWidth = shell.sidebarPoints(windowWidth: width, docked: docked)
                let settledSidebarWidth = shell.sidebarPoints(windowWidth: width, docked: docked, tokens: shell.sidebarWidthSaved)
                let dockWidth = shell.dockPoints(windowWidth: width, sidebar: inlineSidebar ? sidebarWidth : nil)
                let settledDockWidth = shell.dockPoints(windowWidth: width, sidebar: inlineSidebar ? settledSidebarWidth : nil, tokens: shell.dockWidthSaved)
                let sidebarUnder = sidebarIn ? (sidebarWidth + T.windowInset) * shown.sidebar : 0
                let dockUnder = dockIn ? (dockWidth + T.windowInset) * shown.dock : 0
                let settledUnder = docked ? settledDockWidth + T.windowInset : 0
                let settledSidebarUnder = inlineSidebar ? settledSidebarWidth + T.windowInset : 0
                let context = PageContext(sidebarUnder: sidebarUnder, shelfUnder: sidebarIn ? sidebarWidth * shown.sidebar : 0, dockUnder: dockUnder,
                                          dockShown: dockIn ? shown.dock : 0,
                                          hasBar: hasPlayer && mode == .bar, sidebarOpen: sidebarOpen,
                                          pageWidth: width - sidebarUnder - dockUnder,
                                          layoutWidth: width - settledSidebarUnder - settledUnder,
                                          windowHeight: geo.size.height,
                                          pageSince: shell.pageEntry.since,
                                          toggleSidebar: { shell.toggleSidebar() })

                ZStack(alignment: .leading) {
                    backdrop
                    if let connect = shell.connect {
                        ConnectScreen(shell: shell, model: connect, windowHeight: geo.size.height)
                    } else {
                    PageHost(shell: shell).environment(\.pageContext, context)

                    if barIn {
                        // keeps the centre it has without the dock's room, so it only moves down.
                        PlayerBar(shell: shell, canDock: canDock, shown: max(shown.bar, warming.bar))
                            .padding(.leading, sidebarUnder)
                            .frame(maxHeight: .infinity, alignment: .bottom)
                            .allowsHitTesting(shown.bar >= 1)
                            .panelWarming(warming.bar)
                            .transition(.identity)
                    }

                    if sidebarIn {
                        SidebarView(shell: shell, width: sidebarWidth, toggle: { shell.toggleSidebar() })
                            .padding([.leading, .vertical], T.windowInset)
                            .offset(x: -(sidebarWidth + T.windowInset) * (1 - max(shown.sidebar, warming.sidebar)))
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .allowsHitTesting(inlineSidebar)
                            .panelWarming(warming.sidebar)
                            .transition(.identity)
                            .overlay(alignment: .leading) {
                                if !isSnapshot && shown.sidebar >= 1 {
                                    SidebarResizeHandle(shell: shell, windowWidth: width, docked: docked)
                                        .frame(width: T.sidebarGrabW)
                                        .padding(.leading, T.windowInset + sidebarWidth - T.sidebarGrabW / 2)
                                }
                            }
                    }

                    if dockIn {
                        PlayerDock(shell: shell, width: dockWidth, settledWidth: settledDockWidth, windowHeight: geo.size.height)
                            .padding([.trailing, .vertical], T.windowInset)
                            .offset(x: (dockWidth + T.windowInset) * (1 - max(shown.dock, warming.dock)))
                            .frame(maxWidth: .infinity, alignment: .trailing)
                            .allowsHitTesting(docked)
                            .panelWarming(warming.dock)
                            .transition(.identity)
                            .overlay(alignment: .trailing) {
                                if !isSnapshot && shown.dock >= 1 {
                                    DockResizeHandle(shell: shell, windowWidth: width, sidebar: inlineSidebar ? sidebarWidth : nil)
                                        .frame(width: T.dockGrabW)
                                        .padding(.trailing, T.windowInset + dockWidth - T.dockGrabW / 2)
                                }
                            }
                    }

                    if sidebarOpen && !sidebarFits {
                        T.scrim.color
                            .contentShape(Rectangle())
                            .onTapGesture { shell.drawerOpen = false }
                        SidebarView(shell: shell, width: sidebarWidth, toggle: { shell.toggleSidebar() })
                            .shadow(T.shadowFloat)
                    }

                    if shell.expanded {
                        PlayerExpanded(shell: shell, windowSize: geo.size).transition(ExpandClip.transition(fromDock: docked))
                    }

                    if let toast = shell.toast {
                        HStack(spacing: T.toastPad) {
                            Text(toast).lineLimit(1).truncationMode(.tail)
                            if let action = shell.toastAction {
                                ActionButton(title: action.label, small: true) { shell.runToastAction() }
                            }
                        }
                        .font(.navia(T.textMD, .medium))
                        // with an action, the button sits as far from the right edge as from the top and bottom.
                        .padding(.leading, T.toastPad)
                        .padding(.trailing, shell.toastAction == nil ? T.toastPad : (T.toastH - T.buttonSmallH) / 2)
                        .frame(height: T.toastH)
                        // centered in the visible content column (`.toastLane`), from the sidebar's right edge to the dock's left edge.
                        .glassPill()
                        .shadow(T.shadowFloat)
                        .onHover { shell.holdToast(hovered: $0) }
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottom)
                        .padding(.leading, sidebarUnder + T.windowInset * 2)
                        .padding(.trailing, dockUnder + T.windowInset * 2)
                        .padding(.bottom, (hasPlayer && mode == .bar ? T.barH + T.windowInset * 2 : T.windowInset) + T.toastGap)
                    }

                    }

                    if let request = shell.popups.request {
                        SelectPopupLayer(request: request, center: shell.popups, accent: shell.theme.accent, windowSize: geo.size)
                    }
                    if let rules = shell.popups.rules {
                        SmartRulesLayer(request: rules, center: shell.popups, windowSize: geo.size)
                    }
                    if let filter = shell.popups.filter {
                        FilterPopoverLayer(request: filter, center: shell.popups, accent: shell.theme.accent, windowSize: geo.size)
                    }
                    if let quality = shell.popups.quality {
                        QualityPopoverLayer(shell: shell, request: quality, windowSize: geo.size)
                    }
                    if shell.newPlaylistOpen {
                        NewPlaylistLayer(shell: shell)
                    }
                    if let request = shell.airplay.pin {
                        AirPlayPinLayer(shell: shell, request: request).id(request.id)
                    }
                    if let playlist = shell.editingPlaylist {
                        EditPlaylistLayer(playlist: playlist, shell: shell).id(playlist.id)
                    }
                    if let request = shell.shareRequest {
                        ShareDialogLayer(request: request, shell: shell).id(request.id)
                    }
                    if let info = shell.info {
                        GetInfoLayer(target: info, shell: shell)
                    }
                    switch shell.about {
                    case let .artist(id): ArtistAboutLayer(id: id, shell: shell)
                    case let .text(about): AboutTextLayer(about: about, shell: shell)
                    case nil: EmptyView()
                    }
                    if let request = shell.stopDownload {
                        StopDownloadLayer(request: request, shell: shell)
                    }
                    if let request = shell.duplicates {
                        DuplicatesLayer(request: request, shell: shell)
                    }
                    if let user = shell.listenAlongAsk {
                        ListenAlongLayer(user: user, shell: shell)
                    }
                    if let id = shell.listenAlongFriendAsk {
                        ListenAlongFriendLayer(id: id, shell: shell)
                    }
                    if let request = shell.confirmation {
                        ConfirmLayer(request: request) { shell.dismissConfirmation() }
                    }
                }
                .environment(\.theme, shell.theme)
                .id("\(shell.interfaceSize)-\(shell.sessionEpoch)-\(shell.language)")
            }
            .animation(shell.windowShown ? T.ease(T.panelSlide) : nil, value: target)
            .onChange(of: geo.size) { _, _ in shell.popups.close() }
            .onChange(of: sidebarFits, initial: true) { _, fits in shell.sidebarFits = fits }
            .onChange(of: canDock, initial: true) { _, fits in shell.dockFits = fits }
        }
        .ignoresSafeArea()
        .environment(\.marqueeMoves, shell.windowShown && !shell.liveResizing)
        .environment(\.motionLive, shell.windowShown)
        .transaction { [resizing = shell.liveResizing] t in
            if resizing { t.animation = nil; t.disablesAnimations = true }
        }
        .environment(\.colorScheme, .dark)
        .foregroundStyle(T.t1.color)
        .background { if !isSnapshot { WindowConfigurator(translucent: shell.translucencyActive, shell: shell) } }
    }

    /// The window's surface, solid or the blurred desktop with the app's dark color over it.
    @ViewBuilder private var backdrop: some View {
        if isSnapshot {
            Color.clear
        } else if shell.translucencyActive {
            WindowBlur().allowsHitTesting(false).overlay(T.windowTint.color)
        } else {
            T.bg.color
        }
    }

}

/// The system's blur of what's behind the window. `.active` keeps it when the window isn't key, since a player
/// spends most of its time in the background.
struct WindowBlur: NSViewRepresentable {
    func makeNSView(context: Context) -> WindowBlurView {
        let view = WindowBlurView()
        view.material = T.windowMaterial
        view.blendingMode = .behindWindow
        view.state = .active
        return view
    }

    func updateNSView(_ nsView: WindowBlurView, context: Context) {}
}

/// The behind-window blur, which also keeps the window's other backdrops rendered by the app.
///
/// In a translucent window, AppKit hands every in-window backdrop to the window server, which can drop one for a
/// frame and make the top band flicker. Only this view needs the server, since it samples the desktop. The rest are
/// moved back to the app before each Core Animation commit, through the same private layer key as
/// `InAppEffectView`. A macOS without the key keeps its default behavior.
final class WindowBlurView: NSVisualEffectView {
    private var observer: CFRunLoopObserver?

    override func viewWillMove(toWindow newWindow: NSWindow?) {
        // Leaving the window, so drop the server blur.
        if let window, newWindow !== window { WindowServerBlur.set(window, radius: 0) }
        super.viewWillMove(toWindow: newWindow)
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if let window { WindowServerBlur.set(window, radius: T.windowBlurRadius) }
        if let observer { CFRunLoopObserverInvalidate(observer) }
        observer = nil
        guard window != nil, InAppBackdrops.available else { return }
        // Runs before Core Animation's commit at order 2,000,000, after the view updates.
        let o = CFRunLoopObserverCreateWithHandler(nil, CFRunLoopActivity.beforeWaiting.rawValue, true, 1_999_000) { [weak self] _, _ in
            MainActor.assumeIsolated { self?.keepInApp() }
        }
        CFRunLoopAddObserver(CFRunLoopGetMain(), o, .commonModes)
        observer = o
        keepInApp()
    }

    isolated deinit { if let observer { CFRunLoopObserverInvalidate(observer) } }

    private var lastSweep: CFTimeInterval = 0
    private var sweepQueued = false

    /// Sweeps at most every 0.1 s, with one catch-up sweep queued so a still window is covered too. A full walk costs
    /// up to about 2 ms in Debug, too much for every scroll frame.
    private func keepInApp() {
        let now = CACurrentMediaTime()
        let wait = lastSweep + 0.1 - now
        if wait > 0 {
            guard !sweepQueued else { return }
            sweepQueued = true
            DispatchQueue.main.asyncAfter(deadline: .now() + wait) { [weak self] in
                MainActor.assumeIsolated {
                    self?.sweepQueued = false
                    self?.keepInApp()
                }
            }
            return
        }
        guard let root = window?.contentView?.superview?.layer else { return }
        lastSweep = now
        InAppBackdrops.sweep(root, skipping: layer)
    }
}

/// The window server's blur of what's behind a window, through the private SkyLight call
/// `CGSSetWindowBackgroundBlurRadius`. It is looked up at run time, so a system without it gets no extra blur.
enum WindowServerBlur {
    private typealias ConnectionFn = @convention(c) () -> Int32
    private typealias SetFn = @convention(c) (Int32, Int32, Int32) -> Int32
    private static let functions: (ConnectionFn, SetFn)? = {
        guard let handle = dlopen(nil, RTLD_NOW),
              let connection = dlsym(handle, "CGSDefaultConnectionForThread") ?? dlsym(handle, "CGSMainConnectionID"),
              let set = dlsym(handle, "CGSSetWindowBackgroundBlurRadius") else { return nil }
        return (unsafeBitCast(connection, to: ConnectionFn.self), unsafeBitCast(set, to: SetFn.self))
    }()

    @MainActor static func set(_ window: NSWindow, radius: Int) {
        guard let f = functions, window.windowNumber > 0 else { return }
        _ = f.1(f.0(), Int32(window.windowNumber), Int32(max(0, radius)))
    }
}

enum InAppBackdrops {
    private static let key = "windowServerAware"
    private static let backdropClass: AnyClass? = NSClassFromString("CABackdropLayer")
    static let available: Bool = backdropClass.map { ($0 as? NSObject.Type)?.instancesRespond(to: NSSelectorFromString(key)) ?? false } ?? false

    #if DEBUG
    /// For `layerDump=band`: sweep count, layer counts and timings in microseconds.
    @MainActor static var stats = (sweeps: 0, layers: 0, flipped: 0, maxMicros: 0.0, lastMicros: 0.0)
    #endif

    static func isBackdrop(_ layer: CALayer) -> Bool { backdropClass.map { layer.isKind(of: $0) } ?? false }

    /// true if the window server had it.
    @discardableResult @MainActor static func keepInApp(_ layer: CALayer) -> Bool {
        guard available, (layer.value(forKey: key) as? Bool) != false else { return false }
        layer.setValue(false, forKey: key)
        return true
    }

    /// Moves every backdrop under `root`, except under `skipping`, to the app's rendering. Called often, so it is a
    /// plain walk with no extra allocation.
    @MainActor static func sweep(_ root: CALayer, skipping: CALayer?) {
        guard available else { return }
        #if DEBUG
        let start = CACurrentMediaTime()
        var count = 0
        #endif
        var stack: [CALayer] = [root]
        stack.reserveCapacity(256)
        while let layer = stack.popLast() {
            if layer === skipping { continue }
            #if DEBUG
            count += 1
            #endif
            if isBackdrop(layer), keepInApp(layer) {
                #if DEBUG
                stats.flipped += 1
                #endif
            }
            if let subs = layer.sublayers { stack.append(contentsOf: subs) }
        }
        #if DEBUG
        let micros = (CACurrentMediaTime() - start) * 1_000_000
        stats.sweeps += 1
        stats.layers = count
        stats.lastMicros = micros
        stats.maxMicros = max(stats.maxMicros, micros)
        #endif
    }
}

/// Soft light from the artist photo behind the artist page. It reaches under the sidebar and top bar, and scrolls
/// away with the header.
struct PageGlow: View {
    let url: URL
    let windowHeight: CGFloat

    var body: some View {
        ArtworkView(url: url, kind: .artist, pixels: 400, focusY: 0.3)
            .frame(height: windowHeight * 0.82 * T.scale)
            .blur(radius: T.pageGlowBlur, opaque: true)
            .saturation(T.pageGlowSaturation)
            .colorMultiply(Color(white: T.pageGlowBrightness))
            .opacity(T.pageGlowOpacity)
            .mask(LinearGradient(stops: [.init(color: .black, location: 0), .init(color: .black, location: 0.45), .init(color: .clear, location: 1)], startPoint: .top, endPoint: .bottom))
            .frame(maxHeight: .infinity, alignment: .top)
            .allowsHitTesting(false)
    }
}

/// pages take their width from the environment.
struct PageHost: View {
    let shell: ShellModel

    var body: some View {
        page.modifier(PageEntrance(entry: shell.pageEntry.count > 0 ? shell.pageEntry.direction : nil)).id(shell.route.pageKey)
    }

    @ViewBuilder private var page: some View {
        switch shell.route {
        case .songs(let genre): SongsPage(shell: shell, genre: genre)
        case .liked: LikedSongsPage(shell: shell)
        case .settings: SettingsPage(shell: shell)
        case .equalizer: EqualizerPage(shell: shell)
        case .downloads: DownloadsPage(shell: shell)
        case .cachedSongs: CachedSongsPage(shell: shell)
        case .shares: SharesPage(shell: shell)
        case .smartEditor(let id): SmartEditorPage(shell: shell, id: id)
        case .home: HomePage(shell: shell)
        case .albums(let scope): AlbumsPage(shell: shell, scope: scope)
        case .album(let id): AlbumPage(shell: shell, id: id)
        case .artists: ArtistsPage(shell: shell)
        case .artist(let id): ArtistPage(shell: shell, id: id)
        case .artistTop(let id): TopSongsPage(shell: shell, id: id)
        case .playlists: PlaylistsPage(shell: shell)
        case .playlist(let id): PlaylistPage(shell: shell, id: id)
        case .mix(let id): MixPage(shell: shell, id: id)
        case .upcoming(let id): UpcomingPage(shell: shell, id: id)
        case .browse(genres: true): GenresPage(shell: shell)
        case .browse, .search: SearchPage(shell: shell)
        case .discover(let tab): DiscoverPage(shell: shell, tab: tab)
        }
    }
}
