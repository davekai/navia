import AppKit
import NaviaCoreFFI
import Observation
import SwiftUI

enum DiscoverTab: String, CaseIterable, Identifiable {
    case forYou = "for-you", new, charts, radio
    var id: String { rawValue }
    var label: String {
        switch self {
        case .forYou: L("For You")
        case .new: L("New")
        case .charts: L("Charts")
        case .radio: L("Radio")
        }
    }
}

enum Route: Equatable, Hashable {
    /// Search with nothing typed yet. `genres` is the same page reached from the sidebar's Genres row.
    case browse(genres: Bool = false)
    case search(String)
    case home
    case discover(DiscoverTab)
    case albums(AlbumScope)
    case album(String)
    case artists
    case artist(String)
    /// an artist's full Top Songs list.
    case artistTop(String)
    case songs(genre: String?)
    case liked
    case playlists
    case playlist(String)
    case settings
    case equalizer
    case downloads
    case cachedSongs
    case shares
    case smartEditor(String?)
    case mix(String)
    /// Deezer's album id.
    case upcoming(UInt64)

    init(startName: String?) {
        guard let start = startName, !start.isEmpty else { self = .home; return }
        let parts = start.split(separator: ":", maxSplits: 1).map(String.init)
        let value = parts.count > 1 ? parts[1] : nil
        switch parts[0] {
        case "home": self = .home
        case "browse": self = .browse()
        case "genres": self = .browse(genres: true)
        case "search": self = .search(value ?? "")
        case "discover": self = .discover(DiscoverTab(rawValue: value ?? "") ?? .forYou)
        case "albums":
            switch value {
            // `albums:<artist id>[:singles|compilations|appears]` is one section of an artist's albums.
            case let value?:
                let pieces = value.split(separator: ":", maxSplits: 1).map(String.init)
                let section: DiscographySection = switch pieces.count > 1 ? pieces[1] : "" {
                case "singles": .singlesAndEPs
                case "compilations": .compilations
                case "appears": .appearsOn
                default: .albums
                }
                self = .albums(.artist(pieces[0], section))
            default: self = .albums(.all)
            }
        case "album": self = .album(value ?? "")
        case "artists": self = .artists
        case "artist": self = .artist(value ?? "")
        case "artist-top": self = .artistTop(value ?? "")
        case "songs": self = .songs(genre: value)
        case "liked": self = .liked
        case "playlists": self = .playlists
        case "playlist": self = .playlist(value ?? "sample")
        case "settings": self = .settings
        case "equalizer": self = .equalizer
        case "downloads": self = .downloads
        case "cached": self = .cachedSongs
        case "shares": self = .shares
        case "smart": self = .smartEditor(value)
        case "mix": self = .mix(value ?? "")
        case "upcoming": self = .upcoming(UInt64(value ?? "") ?? 0)
        default: self = .home
        }
    }

    /// The page, ignoring its parameters. A page keeps its state while only the parameters change.
    var pageKey: String {
        switch self {
        case .browse(genres: true): "genres"
        case .browse, .search: "search"
        case .home: "home"
        case .discover: "discover"
        case .albums(let scope): "albums-\(scope)"
        case .album(let id): "album-\(id)"
        case .artists: "artists"
        case .artist(let id): "artist-\(id)"
        case .artistTop(let id): "artist-top-\(id)"
        case .songs(let genre): "songs-\(genre ?? "")"
        case .liked: "liked"
        case .playlists: "playlists"
        case .playlist(let id): "playlist-\(id)"
        case .settings: "settings"
        case .equalizer: "equalizer"
        case .downloads: "downloads"
        case .cachedSongs: "cached"
        case .shares: "shares"
        case .smartEditor(let id): "smart-\(id ?? "")"
        case .mix(let id): "mix-\(id)"
        case .upcoming(let id): "upcoming-\(id)"
        }
    }
}

enum BetweenSongs: String { case gapless, crossfade, smart, mix }

enum PlayerMode: String { case dock, bar }
enum PlayerView: String { case lyrics, artwork, queue }

@Observable final class ShellModel {
    /// The signed-in account's models. They change on sign-in, sign-out and server change, and `sessionEpoch` then
    /// rebuilds every page.
    private(set) var library: any LibraryViewModel
    private(set) var songs: any SongsViewModel
    private(set) var player: any PlayerViewModel
    private(set) var sessionEpoch = 0
    /// nil for sample data or before sign-in.
    private(set) var activeLogin: CoreLogin?
    /// replaces the whole shell while it shows.
    private(set) var connect: ConnectModel?
    private(set) var confirmation: ConfirmRequest?
    private(set) var newPlaylistOpen = false
    /// Rules from Save as Smart Playlist, in Navidrome's JSON, with a note on what didn't carry over.
    var smartPrefill: (rules: String, dropped: String?)?
    private(set) var shareRequest: ShareRequest?
    private(set) var info: InfoTarget?
    private(set) var about: AboutSubject?
    private(set) var stopDownload: StopDownloadRequest?
    /// Adding songs a playlist already holds asks first.
    private(set) var duplicates: DuplicatesRequest?
    let songDrag = SongDragModel()
    /// for ⌘I.
    @ObservationIgnored var selectedSong: SongRow?
    /// From Add to Playlist. The new playlist gets these songs and this name, and the page stays put.
    private(set) var newPlaylistSeed: (songs: [SongRow], name: String)?
    /// check runs: the cover slot New Playlist or Edit Details opens on.
    var coverSlotSeed: Int?
    /// check runs: a bump presses the dialog's Create or Save, since a sent click takes the focus.
    var dialogSubmit = 0
    /// rules (Navidrome's JSON) to save as a new smart playlist of the user's own.
    private(set) var smartSaveAs: String?
    let theme = Theme()
    let equalizer = EqualizerStore()
    let popups = PopupCenter()

    private(set) var route: Route
    /// counts page changes, which way the last one went (1 forward, -1 Back) and when, for the page's entrance.
    private(set) var pageEntry: (count: Int, direction: CGFloat, since: Date) = (0, 1, Date())
    @ObservationIgnored private var backStack: [Route] = []
    @ObservationIgnored private var forwardStack: [Route] = []
    private(set) var canGoBack = false
    private(set) var canGoForward = false

    var sidebarPreferred: Bool { didSet { UserDefaults.standard.set(sidebarPreferred, forKey: "sidebarOpen") } }
    var drawerOpen = false
    var playerMode: PlayerMode { didSet { UserDefaults.standard.set(playerMode.rawValue, forKey: "playerMode") } }
    var compactRows = false

    var playerView: PlayerView { didSet { UserDefaults.standard.set(playerView.rawValue, forKey: "playerView") } }
    var expanded = false
    /// show what's left of the song instead of its length; a click on the times switches it.
    var timeShowsRemaining = UserDefaults.standard.bool(forKey: "timeShowsRemaining") {
        didSet { UserDefaults.standard.set(timeShowsRemaining, forKey: "timeShowsRemaining") }
    }
    /// nothing animates while the window is live-resized.
    var liveResizing = false
    /// narrower, the sidebar is a drawer. set by `RootView`.
    var sidebarFits = true
    var sidebarShown: Bool { sidebarFits ? sidebarPreferred : drawerOpen }

    func toggleSidebar() {
        if sidebarFits { sidebarPreferred.toggle() } else { drawerOpen.toggle() }
    }

    /// grows it out of the player and back.
    func setExpanded(_ on: Bool) {
        guard on != expanded, on == false || player.current != nil else { return }
        withAnimation(ExpandClip.animation) { expanded = on }
    }

    /// lyricists, or composers when it has none; nil when the server credits neither.
    func writers(of songID: String) -> String? {
        guard let core = (library as? CoreLibraryModel)?.core, let song = core.songDetails(id: songID) else { return nil }
        for role in ["lyricist", "composer"] {
            let names = song.credits.filter { $0.role == role }.flatMap(\.names).map(\.name).filter { !$0.isEmpty }
            if !names.isEmpty { return names.joined(separator: ", ") }
        }
        return nil
    }

    /// set by `RootView`.
    var dockFits = true
    var playerViewShown: Bool { expanded || (playerMode == .dock && dockFits) }

    /// shows the view (docking the player, or full screen when too narrow), or goes back to artwork if already showing.
    func togglePlayerView(_ view: PlayerView) {
        if playerViewShown && shownPlayerView == view { playerView = .artwork; return }
        playerView = view
        guard !playerViewShown else { return }
        if dockFits { playerMode = .dock } else { setExpanded(true) }
    }
    /// A song without lyrics shows artwork without changing the saved view. lyrics stays while a new song's lyrics load,
    /// since core rows only learn they have lyrics once they arrive.
    var shownPlayerView: PlayerView {
        playerView == .lyrics && (!player.mayHaveLyrics || player.station != nil) ? .artwork : playerView
    }


    /// The saved dock width, in token pixels.
    var dockWidthSaved: CGFloat { didSet { UserDefaults.standard.set(Double(dockWidthSaved), forKey: "dockWidth") } }
    /// The width while dragging, or nil. Saving and other expensive work wait for the end of the drag.
    var dockDragWidth: CGFloat?
    /// before the room rules are applied.
    var dockTokenWidth: CGFloat { dockDragWidth ?? dockWidthSaved }

    /// The saved sidebar width, in token pixels.
    var sidebarWidthSaved: CGFloat { didSet { UserDefaults.standard.set(Double(sidebarWidthSaved), forKey: "sidebarWidth") } }
    var sidebarDragWidth: CGFloat?
    var sidebarTokenWidth: CGFloat { sidebarDragWidth ?? sidebarWidthSaved }

    /// Interface language. `"system"` follows the system, otherwise a language code. `RootView` rebuilds on a change.
    var language: String {
        didSet {
            setLanguage(language)
            UserDefaults.standard.set(language, forKey: "language")
        }
    }

    /// Interface size in percent, 70 to 130. Every size token follows `T.zoom`, and `RootView` rebuilds on a change.
    var interfaceSize: Int {
        didSet {
            T.zoom = CGFloat(interfaceSize) / 100
            UserDefaults.standard.set(interfaceSize, forKey: "interfaceSize")
        }
    }

    var lyricBlur: Bool { didSet { UserDefaults.standard.set(lyricBlur, forKey: "lyricBlur") } }
    /// glow, word lift, springy line moves, gap bars and the previous line's peek; neither the sweep nor the backing line.
    var lyricEffects: Bool { didSet { UserDefaults.standard.set(lyricEffects, forKey: "lyricEffects") } }
    /// off, the current line lights whole, with no rising words.
    var lyricSweep: Bool { didSet { UserDefaults.standard.set(lyricSweep, forKey: "lyricSweep") } }
    /// off, bracketed backing vocals stay inline, as written.
    var backingVocalsLine: Bool { didSet { UserDefaults.standard.set(backingVocalsLine, forKey: "backingVocalsLine") } }

    var betweenSongs: BetweenSongs {
        didSet {
            UserDefaults.standard.set(betweenSongs.rawValue, forKey: "betweenSongs")
            applyCrossfade()
        }
    }
    var crossfadeSeconds: Int {
        didSet {
            UserDefaults.standard.set(crossfadeSeconds, forKey: "crossfadeSeconds")
            applyCrossfade()
        }
    }

    /// Crossfade and Smart both fade. Smart plays a song that continues the album gapless.
    private var crossfadeMs: Int { betweenSongs == .gapless ? 0 : crossfadeSeconds * 1000 }
    private func applyCrossfade() { player.setCrossfade(ms: crossfadeMs, smart: betweenSongs == .smart || betweenSongs == .mix, mix: betweenSongs == .mix) }

    /// nil for the system default.
    var outputDevice: String? {
        didSet {
            UserDefaults.standard.set(outputDevice, forKey: "outputDevice")
            player.setOutputDevice(outputDevice)
            airplay.outputChanged(outputDevice)
        }
    }
    var airplay: AirPlayModel { .shared }
    /// The system's outputs, loaded when Settings, the Equalizer or an Output button first shows.
    private(set) var outputOptions: [(id: String, name: String)] = []
    @ObservationIgnored private var outputsLoaded = false
    @ObservationIgnored private var outputsLoading = false

    /// `refresh` reads the list again, for a device plugged in since.
    func loadOutputs(refresh: Bool = false) {
        guard !outputsLoading, refresh || !outputsLoaded else { return }
        outputsLoading = true
        let sample = CoreMode.useSample
        Task.detached(priority: .utility) { [weak self] in
            let list: [(id: String, name: String)] = sample ? [(id: "speakers", name: "Sample speakers"), (id: "headphones", name: "Sample headphones")] : PlaybackHelpers.systemOutputs()
            await MainActor.run {
                self?.outputsLoading = false
                self?.outputsLoaded = true
                self?.outputOptions = list
            }
        }
    }

    /// The Output menu in the dock and bar. This Mac's outputs come first, then the AirPlay receivers.
    func outputMenu() -> [MenuEntry] {
        let onAirPlay = airplay.activeID != nil
        var entries = [MenuEntry(title: L("System default"), checked: !onAirPlay && outputDevice == nil) { [weak self] in self?.outputDevice = nil }]
        entries += outputOptions.map { option in
            MenuEntry(title: option.name, checked: !onAirPlay && outputDevice == option.id) { [weak self] in self?.outputDevice = option.id }
        }
        entries.append(.divider)
        entries.append(MenuEntry(title: L("AirPlay"), isHeader: true))
        let receivers = airplay.receivers.sorted { a, b in
            let rank = { (r: AirPlayModel.Receiver) in r.support != .ready ? 2 : r.found ? 0 : 1 }
            return rank(a) != rank(b) ? rank(a) < rank(b) : a.name.localizedStandardCompare(b.name) == .orderedAscending
        }
        if receivers.isEmpty {
            entries.append(airplay.searching
                ? MenuEntry(title: L("Looking for AirPlay devices…"), enabled: false)
                : MenuEntry(title: L("No AirPlay devices found"), enabled: false, subtitle: L("Devices on the same network show up here.")))
        }
        for receiver in receivers { entries.append(outputEntry(receiver)) }
        return entries
    }

    private func outputEntry(_ r: AirPlayModel.Receiver) -> MenuEntry {
        let why: String? = switch r.support {
        case .ready: r.found ? nil : L("Not found. It may be asleep.")
        case .restricted: L("Only accepts its owner’s devices")
        case .needsPtp: L("Not supported yet: needs PTP timing")
        case .airplay1: L("Not supported yet: AirPlay 1 only")
        case .password: L("Not supported yet: needs a password")
        }
        if let why { return MenuEntry(title: r.name, enabled: false, reason: why, subtitle: why) }
        let here = airplay.activeID == r.id ? r.state : nil
        let detail = switch here {
        case .connecting: airplay.lost ? L("Reconnecting…") : L("Connecting…")
        case .failed: L("Couldn’t reconnect")
        default: r.needsPin && !r.paired ? L("{model} · Asks for a code once", ["model": r.model]) : r.model
        }
        let airplay = airplay
        return MenuEntry(title: r.name, checked: here == .playing, subtitle: detail.isEmpty ? nil : detail) { airplay.pick(r) }
    }

    func outputLabel(_ id: String) -> String {
        if id == eqOutputDefault { return L("System default") }
        return outputOptions.first { $0.id == id }?.name ?? id
    }

    /// bypass when it's off.
    func applyEqualizer() {
        player.applyEqualizer(equalizer.state.plan(for: outputDevice ?? eqOutputDefault, sampleMode: CoreMode.useSample))
    }

    /// A short message over the page. It clears itself, and hovering keeps it.
    private(set) var toast: String?
    /// The toast's single action, such as Undo. Running it closes the toast.
    private(set) var toastAction: ToastAction?
    @ObservationIgnored private var toastTask: Task<Void, Never>?
    @ObservationIgnored private var heldToast = 0

    struct ToastAction {
        let label: String
        let run: @MainActor () -> Void
    }

    func showToast(_ text: String) { showToast(text, action: nil) }

    /// A toast with an action stays longer, so there's time to reach its button.
    func showToast(_ text: String, action: ToastAction?) {
        heldToast &+= 1
        toast = text
        toastAction = action
        holdToast(hovered: false)
    }

    func holdToast(hovered: Bool) {
        toastTask?.cancel()
        guard !hovered, toast != nil else { return }
        let seconds = toastAction == nil ? T.toast : T.toastAction
        toastTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(seconds))
            guard !Task.isCancelled else { return }
            self?.toast = nil
            self?.toastAction = nil
        }
    }

    func runToastAction() {
        guard let action = toastAction else { return }
        toastTask?.cancel()
        toast = nil
        toastAction = nil
        action.run()
    }

    /// Shows a message until `dismissToast` is called with the returned token. A newer message replaces it.
    func holdToast(_ text: String) -> Int {
        toastTask?.cancel()
        heldToast &+= 1
        toast = text
        toastAction = nil
        return heldToast
    }

    func dismissToast(_ token: Int) {
        guard token == heldToast else { return }
        toast = nil
        toastAction = nil
    }

    private(set) var listenAlongAsk: String?
    /// The same confirm for a Spotify friend, by Discord id.
    private(set) var listenAlongFriendAsk: String?
    func askListenAlong(_ user: String) { listenAlongAsk = user }
    func askListenAlongFriend(_ id: String) { listenAlongFriendAsk = id }
    func closeListenAlong() { listenAlongAsk = nil; listenAlongFriendAsk = nil }

    /// Clears the queue at once. Undo puts the songs back at the top of Next in Queue.
    func clearQueued() {
        let songs = player.clearQueued()
        guard !songs.isEmpty else { return }
        showToast(L("Queue cleared"), action: ToastAction(label: L("Undo")) { [weak self] in self?.player.queueSongs(songs, at: 0, queued: true) })
    }

    /// Clears the rest of the list at once. Undo puts it back right after Next in Queue.
    func clearList() {
        let name = player.queueSource?.title
        guard let undo = player.clearList() else { return }
        let text = name.map { L("Cleared {name}", ["name": $0]) } ?? L("Up Next cleared")
        showToast(text, action: ToastAction(label: L("Undo")) { undo() })
    }

    /// The saved translucency choice. On by default.
    var windowTranslucency: Bool { didSet { UserDefaults.standard.set(windowTranslucency, forKey: "windowTranslucency") } }
    /// overrides the saved choice without changing it.
    var systemReduceTransparency = NSWorkspace.shared.accessibilityDisplayShouldReduceTransparency
    var inFullScreen = false

    /// False while the window can't be seen, such as when minimized, covered, on another Space or hidden.
    /// Nothing redraws for position or animates then. Set through `setWindowShown`.
    private(set) var windowShown = true
    /// so the lyrics land on the current line instead of gliding to it.
    @ObservationIgnored private(set) var shownAt: CFTimeInterval = 0

    /// Called by the window's observer. Coming back catches up in one update without animation, so the first frame
    /// is already at the current position.
    func setWindowShown(_ shown: Bool) {
        guard shown != windowShown else { return }
        if shown { shownAt = CACurrentMediaTime() }
        var transaction = Transaction(animation: nil)
        transaction.disablesAnimations = true
        withTransaction(transaction) {
            windowShown = shown
            player.setViewsLive(shown || miniPlayerShown)
        }
    }

    /// Whether minimizing the main window opens the mini player. Off by default.
    var miniOnMinimize: Bool { didSet { UserDefaults.standard.set(miniOnMinimize, forKey: "miniOnMinimize") } }

    /// The mini player is up. The main window is hidden, but the player's views stay live for the mini player.
    private(set) var miniPlayerShown = false

    func setMiniPlayerShown(_ shown: Bool) {
        guard shown != miniPlayerShown else { return }
        miniPlayerShown = shown
        player.setViewsLive(windowShown || shown)
    }

    /// in full screen there is no desktop behind the window.
    var translucencyActive: Bool { windowTranslucency && !systemReduceTransparency && !inFullScreen }

    /// Scrobbling for this account. On unless turned off. Change it with `chooseScrobble`.
    var scrobblePlays: Bool {
        didSet { library.setReporting(reportingOn) }
    }

    /// The account's stored choice, or on if never set. The legacy `scrobblePlays` key is ignored because it was
    /// written even when nobody chose.
    private static func storedScrobble() -> Bool {
        UserDefaults.standard.object(forKey: AccountPrefs.key("scrobbleChoice")) as? Bool ?? true
    }

    /// Stores the Scrobble plays switch as this account's choice.
    func chooseScrobble(_ on: Bool) {
        UserDefaults.standard.set(on, forKey: AccountPrefs.key("scrobbleChoice"))
        scrobblePlays = on
    }
    /// Discord status for this device. Off by default.
    var discordStatus: Bool {
        didSet {
            UserDefaults.standard.set(discordStatus, forKey: "discordStatus")
            player.setDiscordStatus(discordStatus)
        }
    }
    /// An https address Discord can load the server's covers from. Empty by default.
    /// `applyDiscordPublicAddress` hands it over once entered.
    var discordPublicAddress = "" {
        didSet { UserDefaults.standard.set(discordPublicAddress, forKey: "discordPublicAddress") }
    }
    func applyDiscordPublicAddress() { player.setDiscordPublicAddress(discordPublicAddress) }
    /// Where Discord covers come from: "auto" guesses from the server's address, or "server" or "search".
    var discordCoversFrom = "auto" {
        didSet {
            UserDefaults.standard.set(discordCoversFrom, forKey: "discordCoversFrom")
            player.setDiscordCoversFrom(discordCoversFrom)
        }
    }
    /// Whether the server's address counts as public for covers. Nil while the DNS lookup runs.
    private(set) var serverAddressPublic: (url: String, isPublic: Bool?)?
    func checkServerAddressPublic() {
        guard let url = activeLogin?.url, serverAddressPublic?.url != url else { return }
        serverAddressPublic = (url, nil)
        Task.detached(priority: .utility) { [weak self] in
            let isPublic = serverAddressIsPublic(serverUrl: url)
            await MainActor.run {
                guard let self, self.serverAddressPublic?.url == url else { return }
                self.serverAddressPublic = (url, isPublic)
            }
        }
    }
    /// the sync at launch happens either way.
    var backgroundSync: Bool {
        didSet {
            UserDefaults.standard.set(backgroundSync, forKey: "backgroundSync")
            library.setBackgroundSync(backgroundSync)
        }
    }

    /// Whether plays and stars go to the server, before Private mode is applied.
    private var reportingOn: Bool { !CoreMode.useSample && (CoreLogin.reportOverride || (scrobblePlays && !CoreMode.isTestRun)) }

    init() {
        AppTimeline.noteStart()
        // The User-Agent carries this build's version, so set it before any core or client exists.
        setAppVersion(version: AppUpdates.versionForAgent)
        AppUpdates.shared.startAutomaticChecks()
        if CoreMode.useSample {
            let library = SampleLibrary()
            self.library = library
            songs = library.allSongs
            player = SamplePlayerModel(library: library)
            scrobblePlays = false
            activeLogin = nil
        } else {
            let login = CoreLogin.load()
            AccountPrefs.activate(login)
            let scrobble = Self.storedScrobble()
            let library = CoreLibraryModel(login: login, reportPlays: CoreLogin.reportOverride || (scrobble && !CoreMode.isTestRun))
            _ = library.quality
            self.library = library
            songs = library.allSongs
            player = CorePlayerModel(library: library)
            scrobblePlays = scrobble
            activeLogin = login
            connect = login == nil ? ConnectModel(mode: .firstRun) : nil
        }
        discordStatus = UserDefaults.standard.bool(forKey: "discordStatus")
        discordPublicAddress = UserDefaults.standard.string(forKey: "discordPublicAddress") ?? ""
        discordCoversFrom = UserDefaults.standard.string(forKey: "discordCoversFrom") ?? "auto"
        backgroundSync = UserDefaults.standard.object(forKey: "backgroundSync") as? Bool ?? true
        let defaults = UserDefaults.standard
        language = defaults.string(forKey: "language") ?? "system"
        setLanguage(defaults.string(forKey: "language") ?? "system")
        route = Route(startName: defaults.string(forKey: "startRoute"))
        sidebarPreferred = defaults.object(forKey: "sidebarOpen") as? Bool ?? true
        playerMode = defaults.string(forKey: "playerMode").flatMap(PlayerMode.init(rawValue:)) ?? .dock
        playerView = defaults.string(forKey: "playerView").flatMap(PlayerView.init(rawValue:)) ?? .artwork
        dockWidthSaved = min(T.dockMaxW, max(T.dockMinW, CGFloat(defaults.object(forKey: "dockWidth") as? Double ?? T.dockW)))
        sidebarWidthSaved = min(T.sidebarWMax, max(T.sidebarWMin, CGFloat(defaults.object(forKey: "sidebarWidth") as? Double ?? T.sidebarW)))
        windowTranslucency = defaults.object(forKey: "windowTranslucency") != nil ? defaults.bool(forKey: "windowTranslucency") : true
        interfaceSize = min(130, max(70, defaults.object(forKey: "interfaceSize") as? Int ?? 100))
        T.zoom = CGFloat(min(130, max(70, defaults.object(forKey: "interfaceSize") as? Int ?? 100))) / 100
        lyricBlur = defaults.object(forKey: "lyricBlur") as? Bool ?? true
        lyricEffects = defaults.object(forKey: "lyricEffects") as? Bool ?? true
        lyricSweep = defaults.object(forKey: "lyricSweep") as? Bool ?? true
        backingVocalsLine = defaults.object(forKey: "backingVocalsLine") as? Bool ?? true
        miniOnMinimize = defaults.bool(forKey: "miniOnMinimize")
        betweenSongs = defaults.string(forKey: "betweenSongs").flatMap(BetweenSongs.init(rawValue:)) ?? .gapless
        crossfadeSeconds = min(12, max(1, defaults.object(forKey: "crossfadeSeconds") as? Int ?? 6))
        outputDevice = defaults.string(forKey: "outputDevice")
        airplay.outputChanged(outputDevice)
        configureAirPlay()
        configureSession()
        // Space plays and pauses everywhere except while typing.
        NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            if let self, self.zoomKey(event) { return nil }
            // ⌥⌘M toggles the mini player outside text fields. It's caught here, before the menus, so AppKit's
            // Minimize All, which has the same keys, never gets it.
            if let self, MiniPlayer.isShortcut(event) {
                if !(event.window?.firstResponder is NSText) { MiniPlayer.shared.toggle(shell: self) }
                return nil
            }
            if event.keyCode == 53, let self, self.confirmation != nil { self.dismissConfirmation(); return nil }
            if event.keyCode == 53, let self, self.newPlaylistOpen { self.closeNewPlaylist(); return nil }
            if event.keyCode == 53, let self, self.airplay.pin != nil { self.airplay.cancelPin(); return nil }
            if event.keyCode == 53, let self, self.shareRequest != nil { self.closeShare(); return nil }
            if event.keyCode == 53, let self, self.info != nil { self.closeInfo(); return nil }
            if event.keyCode == 53, let self, self.about != nil { self.closeAbout(); return nil }
            if event.keyCode == 53, let self, self.stopDownload != nil { self.closeStopDownload(); return nil }
            if event.keyCode == 53, let self, self.duplicates != nil { self.closeDuplicates(); return nil }
            if event.keyCode == 53, let self, self.popups.quality != nil { self.popups.closeQuality(unlessPinned: false); return nil }
            if event.keyCode == 53, let self, self.popups.filter != nil { self.popups.close(); return nil }
            // Space and ⌘ arrows are menu shortcuts too; in a text field or on a focused button they stay there.
            if let window = event.window, window.firstResponder is NSText || window.firstResponder is NSButton, NaviaCommands.passesToFocus(event) {
                window.sendEvent(event)
                return nil
            }
            guard event.keyCode == 49, event.modifierFlags.intersection([.command, .control, .option]).isEmpty,
                  !(event.window?.firstResponder is NSText), !(event.window?.firstResponder is NSButton) else { return event }
            self?.player.togglePlayPause()
            return nil
        }
        NSWorkspace.shared.notificationCenter.addObserver(forName: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.systemReduceTransparency = NSWorkspace.shared.accessibilityDisplayShouldReduceTransparency
            }
        }
        // The launch's account. Sign-ins go through `activate`.
        loadDeezerChoice()
        #if DEBUG
        CoreCheck.startIfRequested(shell: self)
        HoverLive.runIfRequested()
        #endif
    }

    /// Command or Control with plus, minus and 0 changes the interface size.
    private func zoomKey(_ event: NSEvent) -> Bool {
        let mods = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        guard mods == .command || mods == .control || mods == [.command, .shift] || mods == [.control, .shift] else { return false }
        switch event.charactersIgnoringModifiers {
        case "=", "+": setInterfaceSize(interfaceSize + 5)
        case "-": setInterfaceSize(interfaceSize - 5)
        case "0": setInterfaceSize(100)
        default: return false
        }
        return true
    }

    /// Wires the AirPlay model to the shell for output changes, pausing when a receiver is lost, and the receiver's
    /// remote buttons and volume.
    private func configureAirPlay() {
        airplay.setOutput = { [weak self] in self?.outputDevice = $0 }
        airplay.onLost = { [weak self] in
            guard let player = self?.player, player.isPlaying else { return }
            player.togglePlayPause()
        }
        airplay.onRemote = { [weak self] command in
            guard let player = self?.player else { return }
            switch command {
            case .playPause: player.togglePlayPause()
            case .play: if !player.isPlaying { player.togglePlayPause() }
            case .pause, .stop: if player.isPlaying { player.togglePlayPause() }
            case .next: player.next()
            case .previous: player.previous()
            case .volumeChanged(let volume): (player as? CorePlayerModel)?.showDeviceVolume(volume)
            }
        }
        #if DEBUG
        if let sample = UserDefaults.standard.string(forKey: "naviaAirPlaySample"), !sample.isEmpty { airplay.applySample(sample) }
        // `-naviaAirPlayMenuDump <file.json>` writes the Output menu's rows once the outputs load, since a native
        // menu's window can't be captured safely.
        if let path = UserDefaults.standard.string(forKey: "naviaAirPlayMenuDump"), !path.isEmpty {
            loadOutputs()
            Task { [weak self] in
                try? await Task.sleep(for: .seconds(3))
                guard let self else { return }
                let rows: [[String: Any]] = self.outputMenu().map { e in
                    ["title": e.title, "subtitle": e.subtitle ?? "", "enabled": e.enabled, "checked": e.checked, "header": e.isHeader, "divider": e.isDivider]
                }
                let menu = makeMenu(self.outputMenu())
                let native = menu.items.map { ["title": $0.title, "subtitle": $0.subtitle ?? "", "header": $0.isSectionHeader, "enabled": $0.isEnabled, "state": $0.state == .on] as [String: Any] }
                let receiver = self.airplay.active
                let report: [String: Any] = ["rows": rows, "native": native, "active": receiver?.name ?? "", "activeState": receiver?.state?.rawValue ?? "", "lost": self.airplay.lost, "pin": self.airplay.pin?.phase == nil ? "" : "\(self.airplay.pin!.phase)"]
                if let data = try? JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys]) { try? data.write(to: URL(fileURLWithPath: path)) }
            }
        }
        #endif
    }

    private func configureSession() {
        applyCrossfade()
        if outputDevice != nil { player.setOutputDevice(outputDevice) }
        applyEqualizer()
        library.setBackgroundSync(backgroundSync)
        player.setDiscordPublicAddress(discordPublicAddress)
        player.setDiscordCoversFrom(discordCoversFrom)
        player.setDiscordStatus(discordStatus)
        player.setViewsLive(windowShown || miniPlayerShown)
        (library as? CoreLibraryModel)?.onNotice = { [weak self] in self?.showToast($0) }
        (library as? CoreLibraryModel)?.onLoginRejected = { [weak self] in self?.loginRejected() }
        (player as? CorePlayerModel)?.sourceProvider = { [weak self] in self?.routeSource() }
    }

    // MARK: Sign-in

    func openChangeServer() {
        connect = ConnectModel(mode: .change, address: activeLogin?.url ?? "", user: activeLogin?.user ?? "")
    }

    /// The server refused the saved login. Change Server opens with the problem, and its Cancel reads Browse
    /// Offline. Does nothing while a sign-in form is open.
    func loginRejected() {
        guard connect == nil else { return }
        let form = ConnectModel(mode: .change, address: activeLogin?.url ?? "", user: activeLogin?.user ?? "")
        form.noteRejected()
        connect = form
    }

    func cancelConnect() {
        guard connect?.mode == .change else { return }
        connect = nil
    }

    /// Saves the login and switches to that account. The old account's data stays in its own folder.
    /// After a first sign-in, the Deezer step follows if this account hasn't chosen.
    func signIn(address: String, user: String, password: String) {
        let saved = LoginKeychain.save(.init(url: address, user: user, password: password))
        let form = connect
        activate(CoreLogin(url: address, user: user, password: password, origin: .keychain))
        if !saved { showToast(L("Couldn't save this login in the Keychain, so Navia will ask again next launch")) }
        if let form, form.mode == .firstRun, deezerChoice == nil, (library as? CoreLibraryModel)?.core != nil {
            form.deezerStep = true
        } else {
            connect = nil
        }
    }

    func answerDeezerStep(_ on: Bool) {
        setDeezerChoice(on)
        connect = nil
    }

    /// This account's Deezer choice. Nil until chosen, which counts as off. It alone decides Navia's own Deezer
    /// requests, regardless of the server's external services setting.
    private(set) var deezerChoice: Bool?

    func setDeezerChoice(_ on: Bool) {
        deezerChoice = on
        // the album tables' Heat column comes and goes with it.
        NotificationCenter.default.post(name: .naviaSongColumnsChanged, object: nil)
        guard let core = (library as? CoreLibraryModel)?.core else { return }
        do { try core.setDeezerRecommendations(on: on) } catch { NSLog("navia: couldn't save the Deezer choice: \(error)") }
    }

    private func loadDeezerChoice() {
        deezerChoice = (library as? CoreLibraryModel)?.core?.deezerRecommendations()
    }

    func requestSignOut() {
        confirm(ConfirmRequest(title: L("Sign out?"), message: L("You'll need to sign in to {host} again. Your downloads stay on this device.", ["host": activeLogin?.host ?? L("this server")]),
                               confirmTitle: L("Sign Out"), compact: true) { [weak self] in self?.signOut() })
    }

    /// Downloads and other per-account data stay on this device.
    func signOut() {
        LoginKeychain.delete()
        activate(nil)
        connect = ConnectModel(mode: .firstRun)
    }

    func openNewPlaylist() { newPlaylistSeed = nil; smartSaveAs = nil; newPlaylistOpen = true }
    func openNewPlaylist(songs: [SongRow], name: String) { newPlaylistSeed = (songs, name); smartSaveAs = nil; newPlaylistOpen = true }
    func openSmartSaveAs(name: String, criteria: String) { newPlaylistSeed = ([], name); smartSaveAs = criteria; newPlaylistOpen = true }
    func closeNewPlaylist() { newPlaylistOpen = false; newPlaylistSeed = nil; smartSaveAs = nil }

    private(set) var editingPlaylist: PlaylistRow?
    func openEditPlaylist(_ playlist: PlaylistRow) { editingPlaylist = playlist }
    func closeEditPlaylist() { editingPlaylist = nil }

    /// fetches its songs first when none are stored yet.
    func playPlaylist(_ playlist: PlaylistRow, shuffle: Bool) {
        Task { @MainActor in
            let songs = await library.playlistSongsToPlay(id: playlist.id)
            guard !songs.isEmpty else { showToast(L("No songs to play")); return }
            player.nextSource = PlaySource(name: playlist.name, link: .playlist(playlist.id))
            playAll(songs, shuffle: shuffle)
        }
    }

    /// asks first, since it's removed from the server for everyone.
    func askDeletePlaylist(_ playlist: PlaylistRow) {
        let downloading = library.synced?.status(.playlist, playlist.id) != nil
        let removed = downloading ? Int(library.synced?.removalPlan(.playlist, playlist.id).remove ?? 0) : 0
        var message = L("It’s removed from the server for everyone.")
        if removed > 0 { message += " " + Ln("Its {count} downloaded song is removed from this device too.", "Its {count} downloaded songs are removed from this device too.", removed) }
        confirm(ConfirmRequest(title: L("Delete “{name}”?", ["name": playlist.name]), message: message, confirmTitle: L("Delete")) { [weak self] in
            self?.deletePlaylist(playlist, stopDownload: downloading)
        })
    }

    private func deletePlaylist(_ playlist: PlaylistRow, stopDownload: Bool) {
        Task { @MainActor in
            if let problem = await library.deletePlaylist(id: playlist.id) { showToast(problem); return }
            if stopDownload { library.synced?.turnOff(.playlist, playlist.id, keepSongs: false) }
            let account = library.accountKey
            SidebarOrder.save(SidebarOrder.load(account: account).filter { $0 != playlist.id }, account: account)
            if route == .playlist(playlist.id) || route == .smartEditor(playlist.id) { go(.playlists) }
        }
    }

    /// A toggle shows at once and goes back if the server refuses.
    private(set) var publicShown: [String: Bool] = [:]
    func isPublic(_ playlist: PlaylistRow) -> Bool { publicShown[playlist.id] ?? playlist.isPublic }

    func togglePublic(_ playlist: PlaylistRow) {
        let next = !isPublic(playlist)
        publicShown[playlist.id] = next
        Task { @MainActor in
            if let problem = await library.updatePlaylistDetails(id: playlist.id, name: nil, comment: nil, isPublic: next) {
                publicShown[playlist.id] = nil
                showToast(problem)
            }
        }
    }
    func openShare(_ request: ShareRequest) { shareRequest = request }
    func closeShare() { shareRequest = nil }

    /// Adds songs to a playlist, from Add to Playlist or a drop. Songs it already holds ask first, otherwise they're
    /// added with a toast. `saved` reports whether the songs went in.
    func addSongs(_ songs: [SongRow], toPlaylist target: PlaylistRef, saved: ((Bool) -> Void)? = nil) {
        guard !songs.isEmpty else { return }
        let held = library.alreadyInPlaylist(id: target.id, songs: songs)
        if !held.isEmpty {
            duplicates = DuplicatesRequest(target: target, songs: songs, fresh: songs.filter { !held.contains($0.id) })
            return
        }
        addSongsNow(songs, to: target, done: L("Added to {name}", ["name": target.name]), saved: saved)
    }

    /// Answers the duplicate warning with all the songs, or only the new ones when `skip`.
    func answerDuplicates(_ request: DuplicatesRequest, skip: Bool) {
        duplicates = nil
        let name = request.target.name
        if skip {
            addSongsNow(request.fresh, to: request.target, done: Ln("Added {count} new song to {name}", "Added {count} new songs to {name}", request.fresh.count, ["name": name]))
        } else {
            addSongsNow(request.songs, to: request.target, done: L("Added to {name}", ["name": name]))
        }
    }

    func closeDuplicates() { duplicates = nil }

    private func addSongsNow(_ songs: [SongRow], to target: PlaylistRef, done: String, saved: ((Bool) -> Void)? = nil) {
        RowFlash.shared.flash(songs.map(\.id))
        Task { @MainActor in
            switch await library.addToPlaylist(id: target.id, songs: songs) {
            case .saved:
                showToast(done)
                saved?(true)
            case .changed(let message), .failed(let message):
                showToast(message)
                saved?(false)
            }
        }
    }

    /// Songs dropped on the queue at Up Next offset `at`, into Next in Queue or the list.
    func queueSongs(_ songs: [SongRow], at: Int, queued: Bool) {
        player.queueSongs(songs, at: at, queued: queued)
        RowFlash.shared.flash(songs.map(\.id))
    }

    /// Songs dropped on the queue tab or the bar's queue button go to the end of Next in Queue.
    func queueSongsAtEnd(_ songs: [SongRow]) {
        player.queueSongs(songs, at: player.queuedCount, queued: true)
        RowFlash.shared.flash(songs.map(\.id))
        showToast(player.sessionOn ? L("Added to the session queue") : L("Added to queue"))
    }
    func confirm(_ request: ConfirmRequest) { confirmation = request }

    /// The Download switch. On keeps the collection on this device and in sync. Off asks first when songs are here.
    func toggleCollectionDownload(_ kind: CollectionKind, id: String, name: String) {
        guard let synced = library.synced else { return }
        guard synced.status(kind, id) != nil else { synced.turnOn(kind, id); return }
        let plan = synced.removalPlan(kind, id)
        if plan.remove == 0 && plan.keep == 0 { synced.turnOff(kind, id, keepSongs: false); return }
        stopDownload = StopDownloadRequest(kind: kind, id: id, name: name, plan: plan)
    }

    func stopCollectionDownload(_ request: StopDownloadRequest, keepSongs: Bool) {
        stopDownload = nil
        library.synced?.turnOff(request.kind, request.id, keepSongs: keepSongs)
    }

    func closeStopDownload() { stopDownload = nil }

    func openInfo(_ target: InfoTarget) { info = target }
    func closeInfo() { info = nil }
    func openAbout(_ artist: String) { about = .artist(artist) }
    func openAbout(_ text: AboutText) { about = .text(text) }
    func closeAbout() { about = nil }

    /// ⌘I closes the sheet when it shows. Otherwise it opens for the selected song, the page's album, playlist or
    /// artist, or the playing song, in that order.
    func infoShortcut() {
        if info != nil { info = nil; return }
        guard confirmation == nil, !newPlaylistOpen, about == nil, connect == nil else { return }
        if let song = selectedSong { info = .song(song.id); return }
        switch route {
        case .album(let id): info = .album(id)
        case .playlist(let id): info = .playlist(id)
        case .artist(let id): info = .artist(id)
        default:
            if player.station == nil, let song = player.current { info = .song(song.id) }
        }
    }
    func dismissConfirmation() { confirmation = nil }

    private func activate(_ login: CoreLogin?) {
        (player as? CorePlayerModel)?.shutdown()
        (library as? CoreLibraryModel)?.shutdown()
        popups.close()
        expanded = false
        drawerOpen = false
        toast = nil
        toastAction = nil
        backStack.removeAll()
        forwardStack.removeAll()
        route = .home
        syncHistory()
        activeLogin = login
        AccountPrefs.activate(login)
        // Reading this before any write means `didSet` can't store it under the wrong account.
        let scrobble = Self.storedScrobble()
        let next = CoreLibraryModel(login: login, reportPlays: CoreLogin.reportOverride || (scrobble && !CoreMode.isTestRun))
        library = next
        songs = next.allSongs
        player = CorePlayerModel(library: next)
        withMutation(keyPath: \.scrobblePlays) { _scrobblePlays = scrobble }
        configureSession()
        sessionEpoch &+= 1
        loadDeezerChoice()
    }

    func setInterfaceSize(_ percent: Int) {
        let next = min(130, max(70, percent))
        if next != interfaceSize { interfaceSize = next }
    }

    /// The dock's width in points, clamped to the allowed token widths. `sidebar` is the sidebar's width in points
    /// when it sits beside the page, or nil when hidden or a drawer.
    func dockPoints(windowWidth: CGFloat, sidebar: CGFloat?, tokens: CGFloat? = nil) -> CGFloat {
        let sidebarW = sidebar.map { $0 + T.windowInset } ?? 0
        let room = (windowWidth - sidebarW - T.windowInset - T.pageMinW) / T.scale
        return T.px(min(tokens ?? dockTokenWidth, max(T.dockMinW, room)))
    }

    /// The sidebar's width in points, kept narrow enough that the page keeps `pageMinW` beside a dock at its
    /// narrowest when `docked`.
    func sidebarPoints(windowWidth: CGFloat, docked: Bool, tokens: CGFloat? = nil) -> CGFloat {
        T.px(min(max(T.sidebarWMin, tokens ?? sidebarTokenWidth), sidebarRoomTokens(windowWidth: windowWidth, docked: docked)))
    }

    func sidebarRoomTokens(windowWidth: CGFloat, docked: Bool) -> CGFloat {
        let dock = docked ? T.px(T.dockMinW) : 0
        let room = (windowWidth - dock - T.windowInset * 2 - T.pageMinW) / T.scale
        return min(T.sidebarWMax, max(T.sidebarWMin, room))
    }

    /// Opens Settings from the account menu, the app menu or ⌘,. The expanded player collapses first, since the page
    /// would be under it.
    func openSettings() {
        guard connect == nil else { return }
        expanded = false
        go(.settings)
    }

    func openAlbums(_ scope: AlbumScope = .all, sort: AlbumSort) {
        albumsSortOnOpen = sort
        if case .albums(scope) = route { albumsSortOnOpen = nil }
        go(.albums(scope))
    }

    /// The sort a link asked the next Albums page to start with. Read once.
    func takeAlbumsSort() -> AlbumSort? {
        defer { albumsSortOnOpen = nil }
        return albumsSortOnOpen
    }

    @ObservationIgnored private var albumsSortOnOpen: AlbumSort?

    /// bumped each time Search is opened, so the field takes focus even when it's already showing.
    private(set) var searchFocus = 0

    /// sidebar pins by account, read from the device the first time each is asked for.
    private var pinsByAccount: [String: SidebarPins] = [:]

    var sidebarPins: SidebarPins { pinsByAccount[library.accountKey] ?? SidebarPins.load(account: library.accountKey) }

    func setSidebarPinned(_ row: PlaylistRef, _ on: Bool) {
        var pins = sidebarPins
        pins.set(row, on: on)
        pinsByAccount[library.accountKey] = pins
        pins.save(account: library.accountKey)
    }

    /// "Pin to Sidebar" or "Unpin from Sidebar"; none for others' private playlists, which have no sidebar section.
    func pinEntry(_ id: String) -> MenuEntry? {
        guard let row = library.playlists.first(where: { $0.id == id }), row.section != .other else { return nil }
        let on = sidebarPins.isPinned(row)
        return MenuEntry(title: on ? L("Unpin from Sidebar") : L("Pin to Sidebar"), symbol: on ? "pin.slash" : "pin") { self.setSidebarPinned(row, !on) }
    }

    func go(_ next: Route) {
        MiniPlayer.shared.close()
        drawerOpen = false
        info = nil
        about = nil
        selectedSong = nil
        if case .browse = next { searchFocus += 1 }
        guard next != route else { return }
        backStack.append(route)
        forwardStack.removeAll()
        route = next
        pageEntry = (pageEntry.count + 1, 1, Date())
        syncHistory()
    }

    /// without a history step, for search typing.
    func replace(_ next: Route) {
        guard next != route else { return }
        route = next
    }

    func goBack() {
        guard let previous = backStack.popLast() else { return }
        forwardStack.append(route)
        route = previous
        pageEntry = (pageEntry.count + 1, -1, Date())
        syncHistory()
    }

    func goForward() {
        guard let next = forwardStack.popLast() else { return }
        backStack.append(route)
        route = next
        pageEntry = (pageEntry.count + 1, 1, Date())
        syncHistory()
    }

    private func syncHistory() {
        canGoBack = !backStack.isEmpty
        canGoForward = !forwardStack.isEmpty
    }

    /// from the top, or from a random song when shuffling.
    func playAll(_ songs: [SongRow], shuffle: Bool = false) {
        guard !songs.isEmpty else { return }
        player.play(songs, at: shuffle ? Int.random(in: 0..<songs.count) : 0, shuffle: shuffle)
    }

    /// Plays a list named by its starter, such as a shelf or an artist radio, rather than by the page.
    func play(_ songs: [SongRow], at index: Int, from source: PlaySource?) {
        if let source { player.nextSource = source }
        player.play(songs, at: index, shuffle: nil)
    }

    /// Starts Artist Radio, named "<artist> Radio". The first request can take seconds.
    func startArtistRadio(_ artist: ArtistRow) {
        guard !radioRunning else { return }
        radioRunning = true
        let token = holdToast(L("Finding similar songs"))
        Task { [weak self] in
            guard let self else { return }
            let songs = await self.library.artistRadio(for: artist)
            self.radioRunning = false
            guard !songs.isEmpty else {
                self.showToast(L("The server has no similar songs for that one"))
                return
            }
            self.dismissToast(token)
            self.player.nextSource = PlaySource(name: artist.name, radio: true)
            self.player.play(songs, at: 0, shuffle: false)
        }
    }

    /// What a list started from this page plays from. Pages without one leave the queue saying "Up Next".
    func routeSource() -> PlaySource? {
        switch route {
        case .album(let id): library.album(id: id).map { PlaySource(name: $0.name, link: route) }
        case .playlist(let id): library.playlist(id: id).map { PlaySource(name: $0.name, link: route) }
        case .artist(let id): library.artist(id: id).map { PlaySource(name: $0.name, link: route) }
        case .artistTop(let id): library.artist(id: id).map { PlaySource(name: L("{artist} Top Songs", ["artist": $0.name]), link: route) }
        case .mix(let id): library.mix(id: id).map { PlaySource(name: $0.title, link: route) }
        case .liked: PlaySource(name: L("Liked Songs"), link: .liked)
        case .songs(let genre): PlaySource(name: genre ?? L("Songs"), link: route)
        default: nil
        }
    }

    /// Asks the server for similar songs and plays them after this one. The first request can take seconds.
    func startSongRadio(_ song: SongRow) {
        guard !radioRunning else { return }
        radioRunning = true
        let token = holdToast(L("Finding similar songs"))
        Task { [weak self] in
            guard let self else { return }
            let queue = await self.library.radioQueue(startingFrom: song)
            self.radioRunning = false
            guard let queue, queue.count > 1 else {
                self.showToast(L("The server has no similar songs for that one"))
                return
            }
            self.dismissToast(token)
            self.player.nextSource = PlaySource(name: song.title, radio: true)
            self.player.play(queue, at: 0, shuffle: false)
        }
    }

    @ObservationIgnored private var radioRunning = false

    /// Plays a song from Search now, and fills Up Next with a radio from it when the server answers.
    func playAsRadio(_ song: SongRow) {
        let library = library
        player.nextSource = PlaySource(name: song.title, radio: true)
        player.playAsRadio(song) { await library.radioSongs(after: song) }
    }

    func goToAlbum(of song: SongRow) { go(.album(song.albumID)) }

    /// Everyone credited on a song, or its display name when the server sends no credits.
    func artistTargets(of song: SongRow) -> [FeaturedArtist] {
        let credited = song.credits.filter { $0.artistID != nil }
        if !credited.isEmpty { return credited }
        guard let artist = library.artist(named: song.artist) else { return [] }
        return [FeaturedArtist(name: artist.name, photoURL: artist.photoURL, artistID: artist.id)]
    }

    func goToArtist(_ target: FeaturedArtist) {
        guard let id = target.artistID else { return }
        go(.artist(id))
    }
}
