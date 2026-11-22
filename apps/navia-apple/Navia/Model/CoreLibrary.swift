import Foundation
import NaviaCoreFFI
import Observation

/// Launch timing for diagnostics. `start` is taken as early as possible.
enum AppTimeline {
    nonisolated(unsafe) private(set) static var start = CFAbsoluteTimeGetCurrent()
    nonisolated(unsafe) private static var marks: [String: Double] = [:]

    static func noteStart() { start = CFAbsoluteTimeGetCurrent() }
    static func mark(_ name: String, rows: Int? = nil) {
        marks[name] = (CFAbsoluteTimeGetCurrent() - start) * 1000
        if let rows { marks["\(name)Rows"] = Double(rows) }
    }
    static var snapshot: [String: Double] { marks }
}

private nonisolated final class CoreBackgroundEvents: CoreListener, @unchecked Sendable {
    let handler: @Sendable (CoreEvent) -> Void
    init(_ handler: @escaping @Sendable (CoreEvent) -> Void) { self.handler = handler }
    func onEvent(event: CoreEvent) { handler(event) }
}

private nonisolated final class DownloadEvents: DownloadListener, @unchecked Sendable {
    let handler: @Sendable (DownloadEventRow) -> Void
    init(_ handler: @escaping @Sendable (DownloadEventRow) -> Void) { self.handler = handler }
    func onEvent(event: DownloadEventRow) { handler(event) }
}

/// The library on the real core. Big lists are read from the local SQLite database by range, and everything else
/// is computed once in the background and cached here.
@Observable final class CoreLibraryModel: LibraryViewModel {
    @ObservationIgnored let core: NaviaCore?
    @ObservationIgnored let shareLinks: ShareLinks?
    @ObservationIgnored let collection: LocalCollection
    @ObservationIgnored private let songsModel: CoreSongsModel
    @ObservationIgnored private var memo: [String: Any] = [:]
    @ObservationIgnored private var loading = Set<String>()
    /// keys whose load was started before the data changed (`forgetMemo`, `refreshMemo`): the value shows when it
    /// lands, and is loaded again on the next read.
    @ObservationIgnored private var outdated = Set<String>()
    @ObservationIgnored private var bumpPending = false
    @ObservationIgnored private var liveLists = NSHashTable<AnyObject>.weakObjects()
    @ObservationIgnored private var albumCache: [String: AlbumRow] = [:]
    @ObservationIgnored private var artistCache: [String: ArtistRow] = [:]
    @ObservationIgnored private let isTestRun = CoreMode.isTestRun
    /// Test runs skip artist biographies and top songs unless `NAVIA_TEST_EXTERNAL=1` is set. The core checks the
    /// same variable.
    @ObservationIgnored private let skipsArtistLookups = CoreMode.isTestRun && ProcessInfo.processInfo.environment["NAVIA_TEST_EXTERNAL"] != "1"

    /// Bumped when background results land, so pages reading through it rebuild.
    private(set) var dataRevision = 0
    private(set) var connected: Bool? {
        // Kept playlists and albums wait while the server is unreachable and catch up when it's back.
        didSet { if let connected, connected != oldValue { core?.setOnline(online: connected) } }
    }
    private(set) var synced: SyncedDownloads?
    private var syncFailed = false
    /// refreshed on each `syncMeter` event.
    private(set) var syncProgress: SyncProgressRow?
    private var syncMeterViews = 0
    var syncMeterOnPage: Bool { syncMeterViews > 0 }
    func syncMeterVisible(_ on: Bool) { syncMeterViews = max(0, syncMeterViews + (on ? 1 : -1)) }
    private func refreshSyncProgress() {
        let running = core?.syncProgress(showAfterMs: 0) != nil
        if running != syncRunning { syncRunning = running }
        let next = core?.syncProgress(showAfterMs: UInt64(T.syncShowAfter * 1000))
        if next?.stage != syncProgress?.stage || next?.done != syncProgress?.done || next?.total != syncProgress?.total || (next == nil) != (syncProgress == nil) {
            syncProgress = next
        }
    }

    var isOffline: Bool { core != nil && (connected == false || offlineSwitch) }
    private(set) var offlineSwitch = false

    var state: LibraryState {
        _ = songsModel.totalCount
        guard core != nil else { return .noServer }
        if songsModel.totalCount > 0 { return .ready }
        if syncFailed { return .syncFailed }
        if syncFinishedObserved { return .ready }
        switch connected {
        case .some(false): return .unreachable
        case .some(true): return .syncing
        case nil: return .opening
        }
    }
    /// A sync has finished at least once, whether or not it found anything.
    private var syncFinishedObserved = false

    @ObservationIgnored private let login: CoreLogin?
    @ObservationIgnored private var qualitySettings: QualitySettings?
    /// Starts watching the metered signal the first time it's read.
    var quality: QualitySettings? {
        if let qualitySettings { return qualitySettings }
        guard let core else { return nil }
        let made = QualitySettings(core: core)
        qualitySettings = made
        return made
    }
    @ObservationIgnored private var playThresholdSettings: PlayThresholdSettings?
    var playThreshold: PlayThresholdSettings? {
        if let playThresholdSettings { return playThresholdSettings }
        guard let core else { return nil }
        let made = PlayThresholdSettings(core: core)
        playThresholdSettings = made
        return made
    }
    @ObservationIgnored private var storageSettings: StorageSettings?
    var storage: StorageSettings? {
        if let storageSettings { return storageSettings }
        guard let core else { return nil }
        let made = StorageSettings(core: core)
        made.onCleared = { [weak self] in
            // Images on screen stay. Anything asked for again comes from the server.
            CoreArt.shared.forgetFiles()
            self?.cachedSongs = []
        }
        made.onSongsChanged = { [weak self] in self?.refreshCachedSongs() }
        storageSettings = made
        return made
    }
    /// Songs in the song cache as of the last check, so rows that play offline aren't dimmed.
    private(set) var cachedSongs: Set<String> = []

    /// Asks the core which songs are cached, off the main thread. Called when the server goes away.
    private func refreshCachedSongs() {
        guard let core else { return }
        Task {
            let ids = await Task.detached(priority: .utility) { core.cachedSongIds() }.value
            cachedSongs = Set(ids)
        }
    }
    var serverLabel: String { login?.host ?? L("Not connected") }
    var reportsPlays: Bool { core?.reportsPlays() ?? false }
    func setReporting(_ on: Bool) { core?.setReporting(on: on) }
    func setBackgroundSync(_ on: Bool) { core?.setBackgroundSync(on: on) }
    var downloadedCount: Int { collection.downloadedSongs.count }

    /// Sizes come from the files. A song that has left the server stays listed, so its file can still be counted
    /// and removed.
    func downloadEntries() async -> [DownloadEntry] {
        guard let core else { return [] }
        let wanted = collection.downloadedSongs
        let rows: [DownloadEntry] = await Task.detached(priority: .userInitiated) {
            let sizes = Dictionary(core.downloadedEntries().map { ($0.id, $0.bytes) }, uniquingKeysWith: { a, _ in a })
            var known: [String: SongRow] = [:]
            for row in core.songsById(ids: Array(sizes.keys)).map({ SongRow(core: $0) }) { known[row.id] = row }
            return sizes.map { id, bytes in
                DownloadEntry(song: known[id] ?? SongRow(id: id, title: L("Unknown song"), artist: L("No longer on the server"), album: "", albumID: "", duration: 0, genre: "", year: nil, plays: 0, isFavorite: false, isDownloaded: true, artworkURL: nil, tintHex: nil), bytes: Int64(bytes))
            }
        }.value
        return rows.filter { wanted.contains($0.song.id) }
            .sorted { $0.song.title.localizedStandardCompare($1.song.title) == .orderedAscending }
            .map { DownloadEntry(song: collection.apply($0.song), bytes: $0.bytes) }
    }

    var allSongs: any SongsViewModel { songsModel }
    var collectionRevision: Int { collection.revision }

    init(login: CoreLogin?, reportPlays: Bool) {
        self.login = login
        var opened: NaviaCore?
        var failure: String?
        if let login {
            do {
                opened = try openCore(serverUrl: login.url, username: login.user, password: login.password,
                                      options: CoreOptions(reportPlays: reportPlays, dataDir: UserDefaults.standard.string(forKey: "naviaDataDir"), testRun: CoreMode.isTestRun))
            } catch {
                failure = "\(error)"
            }
        } else {
            failure = "not signed in"
        }
        if let failure, login != nil { Log.library.error("couldn't open the library: \(String(describing: failure), privacy: .public)") }
        core = opened
        shareLinks = ShareLinks(core: opened)
        let collection = LocalCollection(favorites: [], downloaded: [], favoriteAlbums: [], favoriteArtists: [])
        self.collection = collection
        // Clears a setting older builds left behind.
        UserDefaults.standard.removeObject(forKey: AccountPrefs.key("albumPins"))
        songsModel = CoreSongsModel(core: opened, collection: collection)
        AppTimeline.mark("coreOpened")
        CoreArt.shared.reset(core: opened)
        CoreArt.shared.onTint = { [weak self] in self?.scheduleBump() }
        songsModel.toggle = { [weak self] id in self?.toggleStar(song: id) }
        songsModel.downloader = { [weak self] songs, on in self?.setDownloaded(songs: songs, on) }
        // Clears a setting older builds left behind.
        UserDefaults.standard.removeObject(forKey: AccountPrefs.key("generatedCovers"))
        collection.listen { [weak self] in self?.scheduleBump() }
        loadFavorites()
        guard let opened else { return }
        synced = SyncedDownloads(core: opened)
        watchDay()
        opened.setDownloadListener(listener: DownloadEvents { [weak self] event in
            Task { @MainActor in self?.onDownloadEvent(event) }
        })
        refreshDownloads()
        // before the background starts, so a launch with the switch on never tries the server
        offlineSwitch = UserDefaults.standard.bool(forKey: AccountPrefs.key("offline"))
        opened.setOffline(on: offlineSwitch)
        opened.startBackground(listener: CoreBackgroundEvents { [weak self] event in
            Task { @MainActor in self?.onCoreEvent(event) }
        })
    }

    /// Stops this account's background work before another account takes over.
    func shutdown() {
        retryTask?.cancel()
        retryTask = nil
        core?.shutdown()
        CoreArt.shared.reset(core: nil)
    }

    // MARK: Connection

    /// Messages shown as a toast by the shell. Unused when there is no server.
    @ObservationIgnored var onNotice: ((String) -> Void)?
    /// The server refused the saved login, once per launch, so the shell asks to sign in again.
    @ObservationIgnored var onLoginRejected: (() -> Void)?
    @ObservationIgnored private var probing = false
    @ObservationIgnored private var retryTask: Task<Void, Never>?
    /// The launch ping failed, so the sync and refresh schedule never started.
    @ObservationIgnored private var backgroundNeedsRestart = false

    /// Checks the server after a failure that could mean it's gone, and goes offline if so.
    func noteNetworkTrouble() { Task { await probe() } }

    func retryConnection() async -> Bool { await probe() }

    private var refreshing = false
    /// any library sync runs, shown on the meter or not.
    private var syncRunning = false

    func refreshNow() {
        guard let core, !refreshing else { return }
        refreshing = true
        Task {
            // a change sends `syncDone`, which reloads everything built from the library
            do { _ = try await core.checkForChanges() } catch { Log.library.error("refresh failed: \(String(describing: error), privacy: .public)") }
            refreshMemo("stations")
            scheduleBump()
            refreshing = false
        }
    }

    func isRefreshing(library: Bool) -> Bool { refreshing || library && syncRunning }

    func setOfflineSwitch(_ on: Bool) {
        guard let core, on != offlineSwitch else { return }
        offlineSwitch = on
        UserDefaults.standard.set(on, forKey: AccountPrefs.key("offline"))
        core.setOffline(on: on)
        if on {
            retryTask?.cancel()
            retryTask = nil
            if connected != false { refreshCachedSongs() }
            connected = false
        } else {
            Task { await probe() }
        }
    }

    @discardableResult private func probe() async -> Bool {
        guard let core, !probing else { return connected != false }
        probing = true
        let ok = await core.checkServer()
        probing = false
        let wasOffline = connected == false
        connected = ok
        if !ok && !wasOffline { refreshCachedSongs() }
        if ok {
            retryTask?.cancel()
            retryTask = nil
            if wasOffline { reconnected() }
        } else {
            scheduleRetry()
        }
        return ok
    }

    /// so the row clears by itself when the server returns.
    private func scheduleRetry() {
        guard retryTask == nil, !offlineSwitch else { return }
        retryTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(15))
                guard !Task.isCancelled, let self, self.connected == false else { return }
                await self.probe()
            }
        }
    }

    private func reconnected() {
        if backgroundNeedsRestart, let core {
            backgroundNeedsRestart = false
            core.restartBackground(listener: CoreBackgroundEvents { [weak self] event in
                Task { @MainActor in self?.onCoreEvent(event) }
            })
        }
        reloadLists()
    }

    private func onCoreEvent(_ event: CoreEvent) {
        switch event {
        case .connected(let ok):
            if !ok && connected != false { refreshCachedSongs() }
            connected = ok
            if !ok {
                Log.library.error("can't reach the server")
                backgroundNeedsRestart = true
                scheduleRetry()
            }
        case .syncProgress:
            refreshSyncProgress()
            if songsModel.totalCount == 0 { reloadLists() }
        case .syncMeter:
            refreshSyncProgress()
        case .syncFailed:
            syncFailed = true
        case .loginRejected:
            onLoginRejected?()
        case .forYouChanged:
            refreshMemo("forYou:\(Self.localDay())")
            scheduleBump()
        case .syncDone:
            refreshSyncProgress()
            AppTimeline.mark("syncDone")
            if !syncFinishedObserved { syncFinishedObserved = true }
            forgetMemo()
            albumCache.removeAll()
            artistCache.removeAll()
            loadFavorites()
            reloadLists()
            scheduleBump()
        }
    }

    private func reloadLists() {
        songsModel.reload()
        for case let list as CoreSongsModel in liveLists.allObjects { list.reload() }
        for case let list as CoreAlbumsModel in liveLists.allObjects { list.reload() }
    }

    /// Stars changed while reporting is off, as `kind:id` to a Bool. They stay on this device and are applied over
    /// the server's stars, so a refresh doesn't undo them.
    @ObservationIgnored private var localStars = UserDefaults.standard.dictionary(forKey: AccountPrefs.key("localStars")) as? [String: Bool] ?? [:]

    private func loadFavorites() {
        guard let core else { return }
        let collection = collection
        let local = localStars
        Task.detached(priority: .utility) {
            let order = core.starredSongIds()
            var (songs, albums, artists) = (Set(order), Set(core.starredAlbumIds()), Set(core.starredArtistIds()))
            for (key, on) in local {
                let parts = key.split(separator: ":", maxSplits: 1).map(String.init)
                guard parts.count == 2 else { continue }
                switch parts[0] {
                case "song": if on { songs.insert(parts[1]) } else { songs.remove(parts[1]) }
                case "album": if on { albums.insert(parts[1]) } else { albums.remove(parts[1]) }
                case "artist": if on { artists.insert(parts[1]) } else { artists.remove(parts[1]) }
                default: break
                }
            }
            let localLikes = local.compactMap { $0.value && $0.key.hasPrefix("song:") ? String($0.key.dropFirst(5)) : nil }.sorted()
            let finished = (songs, localLikes + order, albums, artists)
            await MainActor.run { collection.replaceFavorites(songs: finished.0, likedOrder: finished.1, albums: finished.2, artists: finished.3) }
        }
    }

    /// with reporting on, the server holds it.
    private func noteLocalStar(_ key: String, _ on: Bool) {
        guard core?.reportsPlays() == false else { return }
        localStars[key] = on
        UserDefaults.standard.set(localStars, forKey: AccountPrefs.key("localStars"))
    }

    private func scheduleBump() {
        guard !bumpPending else { return }
        bumpPending = true
        Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(24))
            guard let self else { return }
            self.bumpPending = false
            self.dataRevision &+= 1
            // A list opened before the library loaded shows nothing until told.
            for case let list as CorePlaylistsModel in self.liveLists.allObjects where list.isEmpty {
                let rows = self.playlistRows()
                if !rows.isEmpty { list.setRows(rows) }
            }
        }
    }

    /// Returns the cached value, or starts computing it off the main thread and returns `fallback`.
    /// While `refreshMemo` reloads a value, the old one is the fallback.
    private func memoized<T: Sendable>(_ key: String, fallback: T, _ load: @escaping @Sendable () -> T) -> T {
        _ = dataRevision
        if let hit = memo[key] as? T { return hit }
        let fallback = (stale[key] as? T) ?? fallback
        guard let core, loading.insert(key).inserted else { return fallback }
        _ = core
        Task.detached(priority: .utility) { [weak self] in
            let value = load()
            await MainActor.run { self?.memoLanded(key, value) }
        }
        return fallback
    }

    @ObservationIgnored private var stale: [String: Any] = [:]

    /// Reloads a cached value while the old one keeps showing. A playlist change must not empty the sidebar, even
    /// briefly, or its scroll jumps to the top.
    private func refreshMemo(_ key: String) {
        if let old = memo.removeValue(forKey: key) { stale[key] = old }
        if loading.contains(key) { outdated.insert(key) }
    }

    private func memoizedRemote<T: Sendable>(_ key: String, fallback: T, _ load: @escaping @Sendable () async -> T) -> T {
        _ = dataRevision
        if let hit = memo[key] as? T { return hit }
        let fallback = (stale[key] as? T) ?? fallback
        guard core != nil, loading.insert(key).inserted else { return fallback }
        Task.detached(priority: .utility) { [weak self] in
            let value = await load()
            await MainActor.run { self?.memoLanded(key, value) }
        }
        return fallback
    }

    /// Empties the cache after the library changes or the day turns. Loads already running are dropped when
    /// they land and asked again on the next read.
    private func forgetMemo() {
        stale.merge(memo) { _, new in new }
        memo.removeAll()
        outdated.formUnion(loading)
    }

    /// stores a loaded value; one started before the data changed only stands in until it's loaded again.
    private func memoLanded(_ key: String, _ value: Any) {
        loading.remove(key)
        if outdated.remove(key) != nil {
            stale[key] = value
        } else {
            memo[key] = value
            stale[key] = nil
        }
        scheduleBump()
    }

    // MARK: Rows

    func applied(_ album: AlbumRow) -> AlbumRow {
        var out = album
        out.isFavorite = collection.favoriteAlbums.contains(album.id)
        out.tintHex = CoreArt.shared.tints[album.id]
        return out
    }

    func applied(_ artist: ArtistRow) -> ArtistRow {
        var out = artist
        out.isFavorite = collection.favoriteArtists.contains(artist.id)
        return ArtistRow(id: out.id, name: out.name, photoURL: out.photoURL, tintHex: CoreArt.shared.tints["ar-\(artist.id)"], bio: out.bio, isFavorite: out.isFavorite)
    }

    func album(id: String) -> AlbumRow? {
        _ = dataRevision
        if albumCache[id] == nil, let core, let row = core.album(id: id).map({ AlbumRow(core: $0) }) {
            albumCache[id] = row
        }
        guard var row = albumCache[id].map(applied) else { return nil }
        let notes = albumNotes(id)
        if !notes.isEmpty { row.blurb = notes }
        return row
    }

    private func albumNotes(_ id: String) -> String {
        guard let core, !isTestRun else { return "" }
        return memoizedRemote("notes:\(id)", fallback: "") { (try? await core.albumNotes(albumId: id)) ?? "" }
    }

    func artist(id: String) -> ArtistRow? {
        _ = dataRevision
        if artistCache[id] == nil, let core, let row = core.artist(id: id).map({ ArtistRow(core: $0) }) {
            artistCache[id] = row
        }
        guard let row = artistCache[id] else { return nil }
        return withBio(applied(row))
    }

    func artist(named name: String) -> ArtistRow? {
        guard let core, let row = core.artistByName(name: name) else { return nil }
        let artist = ArtistRow(core: row)
        artistCache[artist.id] = artist
        return applied(artist)
    }

    private func withBio(_ artist: ArtistRow) -> ArtistRow {
        guard let core, !skipsArtistLookups else { return artist }
        let info = memoizedRemote("info:\(artist.id)", fallback: CoreArtistInfo(biography: "", similar: [])) {
            let row = try? await core.artistInfo(artistId: artist.id)
            return CoreArtistInfo(biography: row?.biography ?? "", similar: row?.similar ?? [])
        }
        return ArtistRow(id: artist.id, name: artist.name, photoURL: artist.photoURL, tintHex: artist.tintHex, bio: info.biography, isFavorite: artist.isFavorite)
    }

    // MARK: Lists

    func albums(scope: AlbumScope) -> any AlbumsViewModel {
        let model = CoreAlbumsModel(core: core, library: self, scope: scope)
        liveLists.add(model)
        return model
    }

    func artists() -> any ArtistsViewModel { CoreArtistsModel(core: core, library: self) }

    func playlistsList() -> any PlaylistsViewModel {
        let model = CorePlaylistsModel(rows: playlistRows())
        liveLists.add(model)
        let core = core
        Task.detached(priority: .utility) { [weak self] in
            guard core != nil else { return }
            let rows = await MainActor.run { self?.playlistRows() } ?? []
            await MainActor.run { model.setRows(rows) }
        }
        return model
    }

    private func playlistRows() -> [PlaylistRow] {
        guard let core else { return [] }
        let me = core.username()
        let admin = core.isAdmin() == true
        return memoized("playlists", fallback: [PlaylistRow]()) {
            core.playlists().map { p in
                // Only the owner or an admin can edit. Smart playlists' songs stay read-only either way.
                let isSmart = p.smart || p.readonly == true && p.owner.map { $0.lowercased() == me.lowercased() } == true
                // The server's cover, or a grid of the songs' albums when it has none. This never calls `getPlaylist`, which
                // would re-evaluate a smart playlist on the server. So an unopened smart playlist without a cover stays a
                // placeholder.
                let cover = p.coverArt.flatMap { $0.isEmpty ? nil : CoreArt.url(for: $0) }
                let collage = cover != nil ? [] : Self.collage(core.playlistSongs(id: p.id).map { SongRow(core: $0) })
                return PlaylistRow(id: p.id, name: p.name, songCount: Int(p.songCount ?? 0), duration: Int(p.durationSeconds ?? 0),
                                   summary: p.comment ?? "", isSmart: isSmart,
                                   isPublic: p.public ?? false, rules: nil, isOwned: p.owner.map { $0.lowercased() == me.lowercased() } == true || admin, ownerName: p.owner ?? "", isMine: PlaylistSection.owns(p.owner, me: me),
                                   coverURL: cover, collage: collage, canReorder: !isSmart && p.readonly == false)
            }
        }
    }

    func playlist(id: String) -> PlaylistRow? {
        guard var row = playlistRows().first(where: { $0.id == id }) else { return nil }
        row.collage = playlistCollage[id] ?? row.collage
        if row.isSmart, let rules = smartRulesJSON(playlistID: id) {
            let names = playlists.map { (id: $0.id, name: $0.name) }
            row = PlaylistRow(id: row.id, name: row.name, songCount: row.songCount, duration: row.duration, summary: row.summary, isSmart: row.isSmart, isPublic: row.isPublic,
                              rules: smartRuleSummary(rules, playlists: names), isOwned: row.isOwned, ownerName: row.ownerName, isMine: row.isMine, coverURL: row.coverURL, collage: row.collage, canReorder: row.canReorder)
        }
        return row
    }

    @ObservationIgnored private var playlistCollage: [String: [URL]] = [:]

    var playlists: [PlaylistRef] {
        playlistRows().map { PlaylistRef(id: $0.id, name: $0.name, isMine: $0.isMine, isPublic: $0.isPublic, coverURL: $0.coverURL, isSmart: $0.isSmart, collage: playlistCollage[$0.id] ?? $0.collage) }
    }

    var accountKey: String { login.map { NaviaCoreFFI.accountKey(serverUrl: $0.url, username: $0.user) } ?? "local" }

    var genres: [String] { genreRows().map(\.name) }
    private func genreRows() -> [CoreGenre] {
        guard let core else { return [] }
        return memoized("genres", fallback: [CoreGenre]()) { core.genres().map { CoreGenre(name: $0.name, songs: Int($0.songs)) } }
    }
    var genrePage: GenrePage {
        guard let core else { return GenrePage() }
        return memoized("genrePage", fallback: GenrePage()) {
            let page = core.genrePage()
            let entry = { (g: NaviaCoreFFI.GenreEntryRow) in GenreEntry(name: g.name, albums: Int(g.albums), covers: g.coverAlbumIds.compactMap { CoreArt.url(for: $0) }) }
            return GenrePage(top: page.top.map(entry), all: page.all.map(entry), browse: page.browse.map(entry))
        }
    }

    func localSongs(defaultSort: SongSort = .order) -> LocalSongsModel {
        let model = LocalSongsModel(rows: [], collection: collection, defaultSort: defaultSort)
        model.downloader = { [weak self] songs, on in self?.setDownloaded(songs: songs, on) }
        model.toggle = { [weak self] id in self?.toggleStar(song: id) }
        return model
    }

    func songs(forAlbum id: String) -> any SongsViewModel {
        let model = localSongs()
        guard let core else { return model }
        Task.detached(priority: .userInitiated) {
            let rows = core.albumSongs(albumId: id).map { SongRow(core: $0) }
            await MainActor.run { model.setRows(rows) }
        }
        return model
    }

    func songs(forPlaylist id: String) -> any SongsViewModel {
        let model = localSongs()
        guard let core else { return model }
        Task.detached(priority: .userInitiated) { [weak self] in
            let stored = core.playlistSongs(id: id).map { SongRow(core: $0) }
            await MainActor.run { model.setRows(stored); self?.notePlaylistCollage(id, stored) }
            // Test runs never fetch the songs, because fetching a smart playlist changes its stamp on the shared server.
            if (try? await core.refreshPlaylist(id: id)) != nil {
                let refreshed = core.playlistSongs(id: id).map { SongRow(core: $0) }
                await MainActor.run { model.setRows(refreshed); self?.notePlaylistCollage(id, refreshed) }
            }
        }
        return model
    }

    private func notePlaylistCollage(_ id: String, _ rows: [SongRow]) {
        playlistCollage[id] = Self.collage(rows)
        scheduleBump()
    }

    nonisolated private static func collage(_ rows: [SongRow]) -> [URL] {
        var seen = Set<URL>(), collage: [URL] = []
        for row in rows {
            if let url = row.artworkURL, seen.insert(url).inserted { collage.append(url) }
            if collage.count == 4 { break }
        }
        return collage
    }

    func likedSongs() -> any SongsViewModel {
        let model = localSongs(defaultSort: .liked)
        guard let core else { return model }
        model.followFavorites { ids in core.songsById(ids: ids).map { SongRow(core: $0) } }
        return model
    }

    func songs(forGenre genre: String) -> any SongsViewModel {
        let model = CoreSongsModel(core: core, collection: collection, genre: genre)
        model.toggle = { [weak self] id in self?.toggleStar(song: id) }
        model.downloader = { [weak self] songs, on in self?.setDownloaded(songs: songs, on) }
        liveLists.add(model)
        return model
    }

    // MARK: Shelves

    private func ranked(limit: UInt32) -> [CoreRanked] {
        guard let core else { return [] }
        return memoized("ranked:\(limit)", fallback: [CoreRanked]()) {
            core.rankedArtists(query: nil, limit: limit).map { CoreRanked(artist: ArtistRow(core: $0.artist), plays: Int($0.plays), ownAlbums: Int($0.ownAlbums)) }
        }
    }

    /// Artists this user has played, best lately first, by the core's recent-taste score. Rows built on play
    /// history use these, so a new user gets no alphabetical filler.
    private func playedArtists(limit: UInt32) -> [CoreRanked] {
        guard let core else { return [] }
        return memoized("taste:\(limit)", fallback: [CoreRanked]()) {
            core.tasteArtists(limit: limit).map { CoreRanked(artist: ArtistRow(core: $0.artist), plays: Int($0.plays), ownAlbums: Int($0.ownAlbums)) }
        }
    }

    func topArtists(limit: Int) -> [ArtistRow] { playedArtists(limit: 200).prefix(limit).map { applied($0.artist) } }

    private func topSongRows(_ artist: ArtistRow) -> [SongRow] {
        guard let core, !skipsArtistLookups else { return [] }
        return memoizedRemote("top:\(artist.id)", fallback: [SongRow]()) {
            let rows = (try? await core.topSongs(artistId: artist.id, artistName: artist.name)) ?? []
            var seen = Set<String>()
            return rows.map { SongRow(core: $0) }.filter { seen.insert($0.title.lowercased().trimmingCharacters(in: .whitespaces)).inserted }
        }
    }

    /// Essentials from the core (`library::essentials`): it asks the server about the top artists a few at once and
    /// keeps each answer for the day, so a relaunch shows today's tiles at once.
    func essentialsArtists(limit: Int) -> [ArtistRow] {
        guard !isTestRun, let core else { return [] }
        let day = Self.localDay()
        let stored = memoized("essentialsStored:\(day)", fallback: [String]()) { core.essentialArtists() }
        let ids = memoizedRemote("essentials:\(day)", fallback: stored) { await core.refreshEssentialArtists() }
        let byID = Dictionary(playedArtists(limit: 200).map { ($0.artist.id, $0.artist) }, uniquingKeysWith: { first, _ in first })
        return ids.compactMap { byID[$0] }.prefix(limit).map(applied)
    }

    func essentialsSongs(for artist: ArtistRow) -> [SongRow] { topSongRows(artist).map { collection.apply($0) } }
    func topSongs(for artist: ArtistRow) -> [SongRow] { essentialsSongs(for: artist) }

    /// Rotates daily among the top five recent artists with four or more albums. The core picks by local date.
    func worldOf() -> (artist: ArtistRow, albums: [AlbumRow])? {
        guard let core else { return nil }
        let picked = memoized("worldOf:\(Self.localDay())", fallback: [ArtistRow]()) { core.worldOfArtist().map { [ArtistRow(core: $0.artist)] } ?? [] }
        guard let world = picked.first else { return nil }
        let artist = applied(world)
        return (artist, albums(for: artist))
    }

    func onRepeat() -> [SongRow] {
        guard let core else { return [] }
        return memoized("onRepeat", fallback: [SongRow]()) { core.onRepeat().map { SongRow(core: $0) } }.map { collection.apply($0) }
    }

    func backInRotation() -> [ArtistRow] {
        guard let core else { return [] }
        return memoized("rotation", fallback: [ArtistRow]()) { core.backInRotation().map { ArtistRow(core: $0) } }.map(applied)
    }

    /// Asks the server for similar artists off the main thread, at most once per artist per day, so Home paints
    /// first and the row arrives later.
    func becauseYouPlayed() -> (seed: ArtistRow, artists: [ArtistRow])? {
        guard let core else { return nil }
        let found = memoizedRemote("because:\(Self.localDay())", fallback: [ArtistRow]()) {
            guard let row = await core.refreshBecause() else { return [] }
            return [ArtistRow(core: row.seed)] + row.artists.map { ArtistRow(core: $0) }
        }
        guard let seed = found.first, found.count > 1 else { return nil }
        return (applied(seed), found.dropFirst().map(applied))
    }

    /// Today's Artist Radio stations, worked out off the main thread. At most one `getArtistInfo2` per candidate
    /// per day, plus Deezer when enabled.
    func artistStations() -> [ArtistStation] {
        guard let core else { return [] }
        let found = memoizedRemote("stations:\(Self.localDay())", fallback: [ArtistStation]()) {
            await core.artistStations().map { ArtistStation(seed: ArtistRow(core: $0.seed), neighbours: $0.neighbours.map { ArtistRow(core: $0) }) }
        }
        return found.map { ArtistStation(seed: applied($0.seed), neighbours: $0.neighbours.map(applied)) }
    }

    /// Days since 1970 in the user's time zone, from the core's clock.
    nonisolated static func localDay() -> Int { coreLocalDay() }

    func albums(for artist: ArtistRow) -> [AlbumRow] {
        guard let core else { return [] }
        let id = artist.id
        return memoized("albums:\(id)", fallback: [AlbumRow]()) {
            let list = core.albumOrder(order: .year, query: nil, genre: nil, artistId: id)
            return core.albumsByRowid(rowids: list.rowids).map { AlbumRow(core: $0) }
        }.map(applied)
    }

    func discography(for artist: ArtistRow) -> [DiscographyPart]? {
        guard let core else { return [] }
        let id = artist.id
        let key = "discography:\(id)"
        let parts = memoizedRemote(key, fallback: [DiscographyPart]()) {
            guard let found = try? await core.artistDiscography(artistId: id) else { return [DiscographyPart]() }
            return found.sections.map { part in
                DiscographyPart(section: DiscographySection(part.section), albums: core.albumsByRowid(rowids: part.rowids).map { AlbumRow(core: $0) })
            }
        }
        // Nil until the answer lands. An Optional `T` can't mark that, since a missing entry casts to `.some(nil)`.
        guard memo[key] != nil else { return nil }
        return parts.map { DiscographyPart(section: $0.section, albums: $0.albums.map(applied)) }
    }

    func playableSongs(for artist: ArtistRow) -> [SongRow] {
        let top = topSongs(for: artist)
        if !top.isEmpty { return top }
        guard let core else { return [] }
        let id = artist.id
        return memoized("artistSongs:\(id)", fallback: [SongRow]()) {
            core.artistAlbumSongs(artistId: id, limit: 500).map { SongRow(core: $0) }
        }.map { collection.apply($0) }
    }

    func recentAlbums(limit: Int) -> [AlbumRow] {
        guard let core else { return [] }
        return memoized("recent:\(limit)", fallback: [AlbumRow]()) { core.recentAlbums(limit: UInt32(limit)).map { AlbumRow(core: $0) } }.map(applied)
    }

    func topAlbums(limit: Int) -> [AlbumRow] {
        guard let core else { return [] }
        return memoized("topAlbums:\(limit)", fallback: [AlbumRow]()) { core.topAlbums(limit: UInt32(limit)).map { AlbumRow(core: $0) } }.map(applied)
    }

    /// For You from the core, worked out off the main thread. The first visit each day asks the server for the
    /// day's mixes and similar artists. Test runs use only what's stored.
    func forYou() -> ForYouRows? {
        guard let core else { return nil }
        // Not an optional value: a missing memo entry casts to an optional's nil and would count as found.
        var rows = memoized("forYou:\(Self.localDay())", fallback: ForYouRows(loaded: false)) { ForYouRows(core: core.forYou()) }
        guard rows.loaded else { return nil }
        rows.newToYou = rows.newToYou.map(applied)
        rows.unplayed = rows.unplayed.map(applied)
        rows.onThisDay = rows.onThisDay.map(applied)
        rows.notPlayedRecently = rows.notPlayedRecently.map(applied)
        rows.forgotten = rows.forgotten.map { collection.apply($0) }
        rows.mixes = rows.mixes.map { MixInfo(id: $0.id, isGenre: $0.isGenre, name: $0.name, songs: $0.songs.map { collection.apply($0) }, artists: $0.artists) }
        return rows
    }

    func mix(id: String) -> MixInfo? { forYou()?.mixes.first { $0.id == id } }

    func songs(forMix mix: MixInfo) -> any SongsViewModel {
        let model = LocalSongsModel(rows: mix.songs, collection: collection, defaultSort: .order)
        model.downloader = { [weak self] songs, on in self?.setDownloaded(songs: songs, on) }
        model.toggle = { [weak self] id in self?.toggleStar(song: id) }
        return model
    }

    func songs(forArtistTop rows: [SongRow]) -> any SongsViewModel {
        let model = LocalSongsModel(rows: rows, collection: collection, defaultSort: .order, sortable: false)
        model.downloader = { [weak self] songs, on in self?.setDownloaded(songs: songs, on) }
        model.toggle = { [weak self] id in self?.toggleStar(song: id) }
        return model
    }

    /// day-keyed rows are worked out again when the day changes; the core's clock decides, so they turn over together.
    private func watchDay() {
        Task { [weak self] in
            var day = Self.localDay()
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(30))
                guard let self else { return }
                let today = Self.localDay()
                guard today != day else { continue }
                day = today
                self.forgetMemo()
                self.scheduleBump()
            }
        }
    }

    func latestSongs(limit: Int) -> [SongRow] {
        guard let core else { return [] }
        return memoized("latestSongs:\(limit)", fallback: [SongRow]()) {
            core.recentAlbums(limit: UInt32(limit)).compactMap { core.albumSongs(albumId: $0.id).first.map { SongRow(core: $0) } }
        }.map { collection.apply($0) }
    }

    func topSongs(limit: Int) -> [SongRow] {
        guard let core else { return [] }
        return memoized("topSongs:\(limit)", fallback: [SongRow]()) { core.topSongsLocal(limit: UInt32(limit)).map { SongRow(core: $0) } }.map { collection.apply($0) }
    }

    var stations: [RadioStation] {
        guard let core else { return [] }
        return memoizedRemote("stations", fallback: [RadioStation]()) {
            ((try? await core.stations()) ?? []).map { RadioStation(name: $0.name, url: $0.url) }
        }
    }

    /// Radio tiles ask for similar songs one artist at a time, like Essentials.
    func radioArtists(limit: Int) -> [ArtistRow] {
        guard !isTestRun else { return [] }
        _ = dataRevision
        // Recently played artists first, then the rest in library order.
        let lately = playedArtists(limit: 40)
        let seen = Set(lately.map(\.artist.id))
        let order = lately + ranked(limit: 40).filter { !seen.contains($0.artist.id) }
        return order.prefix(40).filter { ($0.similarCount(in: memo) ?? 0) >= 5 }.prefix(limit).map { applied($0.artist) }
    }

    func similarSongs(for artist: ArtistRow) -> [SongRow] {
        guard let core, !isTestRun else { return [] }
        let id = artist.id
        return memoizedRemote("similar:\(id)", fallback: [SongRow]()) {
            ((try? await core.similarSongs(artistId: id, limit: 40)) ?? []).map { SongRow(core: $0) }
        }.map { collection.apply($0) }
    }

    /// Asked off the main thread, at most once a day per artist. Deezer's related artists are merged in when
    /// enabled. Test runs only read what's stored.
    func similarArtists(for artist: ArtistRow) -> [ArtistRow] {
        guard let core else { return [] }
        let id = artist.id
        return memoizedRemote("similarArtists:\(id)", fallback: [ArtistRow]()) {
            await core.similarArtists(artistId: id).map { ArtistRow(core: $0) }
        }.map(applied)
    }

    func moreBy(album: AlbumRow) -> MoreBy? {
        guard let core else { return nil }
        let id = album.id
        // Not optional, because a missing memo entry would cast to nil and count as found.
        let found = memoized("moreBy:\(id)", fallback: [MoreBy]()) {
            core.moreBy(albumId: id).map { [MoreBy(artist: ArtistRow(core: $0.artist), albums: $0.albums.map { AlbumRow(core: $0) }, section: DiscographySection($0.section))] } ?? []
        }
        guard let more = found.first else { return nil }
        return MoreBy(artist: applied(more.artist), albums: more.albums.map(applied), section: more.section)
    }

    func albumHeat(album: AlbumRow) -> [String: UInt64] {
        guard let core, core.deezerRecommendations() == true else { return [:] }
        let id = album.id
        return memoizedRemote("albumHeat:\(id)", fallback: Self.ranks(core.keptAlbumHeat(albumId: id))) {
            Self.ranks(await core.albumHeat(albumId: id))
        }
    }

    func reaskMissingHeat(album: AlbumRow) {
        let key = "albumHeat:\(album.id)"
        guard (memo[key] as? [String: UInt64])?.isEmpty == true else { return }
        memo[key] = nil
        _ = albumHeat(album: album)
    }

    private nonisolated static func ranks(_ rows: [SongHeatRow]) -> [String: UInt64] {
        Dictionary(rows.map { ($0.songId, $0.rank) }, uniquingKeysWith: max)
    }

    /// test runs only read what's kept.
    func upcoming(for artist: ArtistRow) -> [UpcomingAlbum] {
        guard let core, core.deezerRecommendations() == true else { return [] }
        let id = artist.id
        return memoizedRemote("upcoming:\(id)", fallback: [UpcomingAlbum]()) { await core.upcomingForArtist(artistId: id).map(UpcomingAlbum.init(core:)) }
    }

    func upcomingHome() -> [UpcomingAlbum] {
        guard let core, core.deezerRecommendations() == true else { return [] }
        return memoizedRemote("upcomingHome", fallback: [UpcomingAlbum]()) { await core.upcomingHome().map(UpcomingAlbum.init(core:)) }
    }

    func upcomingMatching(_ query: String) -> [UpcomingAlbum] {
        guard let core, core.deezerRecommendations() == true else { return [] }
        return core.upcomingMatching(query: query).map(UpcomingAlbum.init(core:))
    }

    func upcomingPage(_ id: UInt64) -> UpcomingPageState {
        guard let core, core.deezerRecommendations() == true else { return .missing }
        let page = memoizedRemote("upcomingPage:\(id)", fallback: UpcomingPageState.loading) {
            switch await core.upcomingPage(id: id) {
            case .upcoming(let album)?: .upcoming(UpcomingAlbum(core: album))
            case .onServer(let albumId)?: .onServer(albumId)
            case nil: .missing
            }
        }
        guard case .upcoming(var album) = page else { return page }
        album.tracks = album.tracks.map { album.unavailable.contains($0.id) ? $0 : collection.apply($0) }
        return .upcoming(album)
    }

    func songs(forUpcoming album: UpcomingAlbum) -> any SongsViewModel {
        let model = LocalSongsModel(rows: album.tracks, collection: collection, defaultSort: .order)
        model.downloader = { [weak self] songs, on in self?.setDownloaded(songs: songs, on) }
        model.toggle = { [weak self] id in self?.toggleStar(song: id) }
        return model
    }

    func songs(by artist: ArtistRow, matching query: String) -> [SongRow] {
        guard let core else { return [] }
        let id = artist.id
        let rows = memoized("artistSongs2:\(id)", fallback: [SongRow]()) {
            core.artistAlbumSongs(artistId: id, limit: 2000).map { SongRow(core: $0) }
        }
        let needle = query.lowercased()
        return Array(rows.lazy.filter { "\($0.title) \($0.album)".lowercased().contains(needle) }.prefix(12)).map { collection.apply($0) }
    }

    func radioQueue(for song: SongRow) -> [SongRow]? {
        guard let artist = artist(named: song.artist) else { return nil }
        let similar = similarSongs(for: artist).filter { $0.id != song.id }
        return similar.isEmpty ? nil : [song] + similar
    }

    /// The query runs off the main thread. Only flags and ordering are applied here.
    func searchResults(_ query: String, sort: SearchSort, favoritesOnly: Bool) async -> SearchResults {
        guard let core else { return SearchResults(topArtist: nil, topArtistSongs: [], artists: [], albums: [], songs: []) }
        let found = await Task.detached(priority: .userInitiated) {
            let found = core.search(query: query, limitPerKind: 20)
            return (found.artists.map { ArtistRow(core: $0) }, found.albums.map { AlbumRow(core: $0) }, found.songs.map { SongRow(core: $0) }, found.best)
        }.value
        var artists = found.0.map(applied)
        var albums = found.1.map(applied)
        var songs = found.2.map { collection.apply($0) }
        if favoritesOnly {
            artists = artists.filter(\.isFavorite)
            albums = albums.filter(\.isFavorite)
            songs = songs.filter(\.isFavorite)
        }
        switch sort {
        case .name: albums.sort { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
        case .year: albums.sort { ($0.year ?? 0) > ($1.year ?? 0) }
        case .relevance: break
        }
        // The top result is an artist only when an artist is the best match overall.
        return SearchResults(topArtist: found.3 == .artist ? artists.first : nil, topArtistSongs: [], artists: artists, albums: albums, songs: Array(songs.prefix(12)))
    }


    // MARK: Smart playlists

    func smartRulesProblem(name: String, criteria: String) -> String? {
        guard core != nil else { return L("Saving needs the server.") }
        return NaviaCoreFFI.smartRulesProblem(name: name, criteria: criteria).map { L($0) }
    }

    func saveSmartPlaylist(id: String?, name: String, criteria: String) async -> Result<String, SmartSaveFailure> {
        guard let core else { return .failure(SmartSaveFailure(message: L("Saving needs the server."))) }
        let result: SmartSaveResult
        do { result = try await core.saveSmartPlaylist(id: id, name: name, criteria: criteria) } catch { return .failure(SmartSaveFailure(message: L("Couldn't save the playlist. {problem}", ["problem": error.localizedDescription]))) }
        if let problem = result.problem { return .failure(SmartSaveFailure(message: L(problem))) }
        // The rules as the server stored them. The core stores the evaluated songs.
        if let saved = result.id, let rules = result.rules { memo["rules:\(saved)"] = rules }
        playlistChanged()
        return .success(result.id ?? id ?? "")
    }

    func smartRulesJSON(playlistID: String) -> String? {
        // Test runs never read rules, since that logs in, except for a playlist the test made itself.
        guard let core, !isTestRun || ProcessInfo.processInfo.environment["NAVIA_TEST_PLAYLIST"] == playlistID else { return nil }
        let text = memoizedRemote("rules:\(playlistID)", fallback: "") { ((try? await core.playlistRules(id: playlistID)) ?? nil) ?? "" }
        return text.isEmpty ? nil : text
    }

    func smartPreview(editor: NaviaSmartEditor, maxRows: Int) async -> (preview: SmartPreviewResult, songs: [SongRow]) {
        guard let core else { return (SmartPreviewResult(rowids: [], matched: 0, sampled: 0, unchecked: [], checkable: false, sortUnchecked: []), []) }
        let result: (SmartPreviewResult, [SongRow]) = await Task.detached(priority: .userInitiated) {
            let preview = editor.previewLibrary(core: core, max: UInt32(maxRows))
            return (preview, core.songsByRowid(rowids: preview.rowids).map { SongRow(core: $0) })
        }.value
        return (result.0, result.1.map { collection.apply($0) })
    }

    // MARK: Downloads and radio

    @ObservationIgnored private var noRadio = Set<String>()
    private(set) var downloadProgressByID: [String: Double] = [:]

    func radioQueue(startingFrom song: SongRow) async -> [SongRow]? {
        guard let core else { return nil }
        #if DEBUG
        // A slow fake lookup, to check what the app shows while waiting.
        if isTestRun, UserDefaults.standard.integer(forKey: "naviaTestRadioMs") > 0 {
            try? await Task.sleep(for: .milliseconds(UserDefaults.standard.integer(forKey: "naviaTestRadioMs")))
            return [song] + core.albumSongs(albumId: song.albumID).map { SongRow(core: $0) }.filter { $0.id != song.id }.prefix(5).map { collection.apply($0) }
        }
        #endif
        let rows = await radioSongs(after: song)
        return rows.isEmpty ? nil : [song] + rows
    }

    /// Recommended songs under a playlist, ranked by the core. Read-only.
    func recommender(forPlaylist id: String) -> (() async -> [SongRow])? {
        guard let core else { return nil }
        let recommender = core.makeRecommender(playlistId: id)
        return { [collection] in await recommender.nextBatch().map { collection.apply(SongRow(core: $0)) } }
    }

    /// Similar songs from the core, without the song or its other versions, then the same artists' songs.
    /// Read-only, so test runs may ask too.
    func radioSongs(after song: SongRow) async -> [SongRow] {
        guard let core else { return [] }
        let rows = await core.radioSongs(songId: song.id).map { collection.apply(SongRow(core: $0)) }
        if rows.isEmpty { noRadio.insert(song.id) }
        return rows
    }

    func artistRadio(for artist: ArtistRow) async -> [SongRow] {
        guard let core, !isTestRun else { return [] }
        return await core.artistRadio(artistId: artist.id, artistName: artist.name).map { collection.apply(SongRow(core: $0)) }
    }

    func knownNoRadio(_ song: SongRow) -> Bool { noRadio.contains(song.id) }

    /// Only a finished file counts as downloaded.
    func refreshDownloads() {
        guard let core else { return }
        let collection = collection
        Task.detached(priority: .utility) {
            let done = core.downloadedSongIds()
            await MainActor.run { collection.replaceDownloads(Set(done)) }
        }
    }

    func onDownloadEvent(_ event: DownloadEventRow) {
        switch event {
        case .progress(let id, let done, let total):
            let fraction = total.map { $0 > 0 ? Double(done) / Double($0) : 0 } ?? 0
            downloadProgressByID[id] = max(fraction, downloadProgressByID[id] ?? 0)
        case .finished(let id, _):
            downloadProgressByID[id] = nil
            collection.setDownloaded([id], true)
        case .failed(let id, let message):
            downloadProgressByID[id] = nil
            collection.refreshRows()
            Log.library.error("download failed: \(message, privacy: .public)")
        case .removed(let id):
            downloadProgressByID[id] = nil
            collection.setDownloaded([id], false)
        }
    }

    // MARK: Favorites

    /// Stars change on the server, so they wait while offline. Explains why instead of doing nothing.
    private func starsAvailable() -> Bool {
        guard isOffline else { return true }
        onNotice?(L("Favorites need the server. Try again when you're back online."))
        return false
    }

    func toggleFavorite(album id: String) {
        guard starsAvailable() else { return }
        collection.toggleFavorite(album: id)
        noteLocalStar("album:\(id)", collection.favoriteAlbums.contains(id))
        core?.sendStar(table: .album, id: id, on: collection.favoriteAlbums.contains(id))
    }

    func toggleFavorite(artist id: String) {
        guard starsAvailable() else { return }
        collection.toggleFavorite(artist: id)
        noteLocalStar("artist:\(id)", collection.favoriteArtists.contains(id))
        core?.sendStar(table: .artist, id: id, on: collection.favoriteArtists.contains(id))
    }

    private func toggleStar(song id: String) {
        guard starsAvailable() else { return }
        collection.toggleFavorite(song: id)
        noteLocalStar("song:\(id)", collection.favoriteSongs.contains(id))
        core?.sendStar(table: .song, id: id, on: collection.favoriteSongs.contains(id))
    }

    func toggleFavorite(song: SongRow) { toggleStar(song: song.id) }
    func isFavorite(song: SongRow) -> Bool { collection.favoriteSongs.contains(song.id) }
    func isDownloaded(song: SongRow) -> Bool { collection.downloadedSongs.contains(song.id) }
    func playsOffline(song: SongRow) -> Bool {
        isDownloaded(song: song) || cachedSongs.contains(song.id) || core?.isSongCached(id: song.id) == true
    }
    func downloadProgress(song: SongRow) -> Double? { downloadProgressByID[song.id] }
    func setDownloaded(songs: [SongRow], _ downloaded: Bool) {
        guard let core else { return }
        let ids = songs.map(\.id)
        if downloaded {
            let fresh = ids.filter { !collection.downloadedSongs.contains($0) }
            for id in fresh where downloadProgressByID[id] == nil { downloadProgressByID[id] = 0 }
            collection.refreshRows()
            Task.detached(priority: .utility) { _ = core.downloadSongs(ids: fresh) }
        } else {
            for id in ids { downloadProgressByID[id] = nil }
            collection.setDownloaded(ids, false)
            Task.detached(priority: .utility) { core.cancelDownloads(ids: ids); core.removeDownloads(ids: ids) }
        }
    }
    func savePlaylistOrder(id: String, expected: [String], order: [String]) async -> PlaylistEdit {
        guard let core else { return .failed(L("Test and read-only runs can't change playlists.")) }
        let result: PlaylistEditResult
        do { result = try await core.savePlaylistOrder(id: id, expected: expected, order: order) } catch {
            return .failed(L("Couldn't save the new order: {error}", ["error": error.localizedDescription]))
        }
        return edited(result, changed: L("This playlist changed on the server. Showing the latest order."), failed: { L("Couldn't save the new order: {error}", ["error": $0]) })
    }

    func playlistTargets() -> [PlaylistRef] {
        guard let core else { return [] }
        return core.playlistTargets().map { PlaylistRef(id: $0.id, name: $0.name) }
    }

    func addToPlaylist(id: String, songs: [SongRow]) async -> PlaylistEdit {
        guard let core else { return .failed(L("Playlists can't be changed without the server.")) }
        let name = playlist(id: id)?.name ?? ""
        let result: PlaylistEditResult
        do { result = try await core.addToPlaylist(id: id, songIds: songs.map(\.id)) } catch {
            return .failed(L("Couldn't add to {name}: {error}", ["name": name, "error": error.localizedDescription]))
        }
        playlistChanged()
        return edited(result, changed: "", failed: { L("Couldn't add to {name}: {error}", ["name": name, "error": $0]) })
    }

    func alreadyInPlaylist(id: String, songs: [SongRow]) -> Set<String> {
        guard let core else { return [] }
        return Set(core.alreadyInPlaylist(id: id, songIds: songs.map(\.id)))
    }

    func removeFromPlaylist(id: String, expected: [String], places: [Int]) async -> PlaylistEdit {
        guard let core else { return .failed(L("Playlists can't be changed without the server.")) }
        let name = playlist(id: id)?.name ?? ""
        let result: PlaylistEditResult
        do { result = try await core.removeFromPlaylist(id: id, expected: expected, indexes: places.map { UInt32($0) }) } catch {
            return .failed(L("Couldn't remove from {name}: {error}", ["name": name, "error": error.localizedDescription]))
        }
        playlistChanged()
        return edited(result, changed: L("This playlist changed on the server, so nothing was removed. Showing the latest songs."), failed: { L("Couldn't remove from {name}: {error}", ["name": name, "error": $0]) })
    }

    /// The stored playlist changed, so lists and pages read it again.
    private func playlistChanged() {
        refreshMemo("playlists")
        scheduleBump()
    }

    private func edited(_ result: PlaylistEditResult, changed: String, failed: (String) -> String) -> PlaylistEdit {
        switch result {
        case .saved: .saved
        case .writesDisabled: .failed(L("Test and read-only runs can't change playlists."))
        case .readOnly: .failed(L("This playlist can't be changed."))
        case .changed: .changed(changed)
        case .failed(let message): .failed(failed(message))
        }
    }

    func storedPlaylistSongs(id: String) async -> [SongRow]? {
        guard let core else { return nil }
        return await Task.detached(priority: .userInitiated) { core.playlistSongs(id: id).map { SongRow(core: $0) } }.value
    }

    func createPlaylist(name: String, songs: [SongRow]) async throws -> String? {
        guard let core else { throw CoreCreateError.noServer }
        let created = songs.isEmpty ? try await core.createPlaylist(name: name) : try await core.createPlaylistWith(name: name, songIds: songs.map(\.id))
        if let problem = created.problem { throw CoreCreateError.failed(problem) }
        refreshMemo("playlists")
        scheduleBump()
        return created.id
    }

    var canChangePlaylistImages: Bool { core?.canChangePlaylistImages() ?? false }

    func setPlaylistImage(id: String, path: String) async -> String? {
        guard let core else { return L("Changing a cover needs the server.") }
        let problem = await Self.problem { try await core.setPlaylistImage(id: id, path: path) }
        refreshMemo("playlists")
        scheduleBump()
        return problem
    }

    func updatePlaylistDetails(id: String, name: String?, comment: String?, isPublic: Bool?) async -> String? {
        guard let core else { return L("Editing a playlist needs the server.") }
        let problem: String?
        do { problem = try await core.updatePlaylistDetails(id: id, name: name, comment: comment, public: isPublic) } catch { problem = "\(error)" }
        refreshMemo("playlists")
        scheduleBump()
        return problem.map(Self.detailsProblem)
    }

    func playlistSongsNow(id: String) -> [SongRow] { core?.playlistSongs(id: id).map { SongRow(core: $0) } ?? [] }

    func playlistSongsToPlay(id: String) async -> [SongRow] {
        guard let core else { return [] }
        return await Task.detached(priority: .userInitiated) {
            let stored = core.playlistSongs(id: id)
            if !stored.isEmpty { return stored.map { SongRow(core: $0) } }
            _ = try? await core.refreshPlaylist(id: id)
            return core.playlistSongs(id: id).map { SongRow(core: $0) }
        }.value
    }

    func deletePlaylist(id: String) async -> String? {
        guard let core else { return L("Deleting a playlist needs the server.") }
        let problem: String?
        do { problem = try await core.deletePlaylist(id: id) } catch { problem = "\(error)" }
        refreshMemo("playlists")
        scheduleBump()
        guard let problem else { return nil }
        if problem.hasPrefix("Test and read-only") { return L("Test and read-only runs can't change playlists.") }
        let prefix = "The server didn't delete it: "
        return problem.hasPrefix(prefix) ? L("The server didn't delete it: {error}", ["error": String(problem.dropFirst(prefix.count))]) : problem
    }

    /// The core's English reasons, in the user's language.
    private static func detailsProblem(_ text: String) -> String {
        if text.hasPrefix("Test and read-only") { return L("Test and read-only runs can't change playlists.") }
        let prefix = "The server didn't save the change: "
        if text.hasPrefix(prefix) { return L("The server didn't save the change: {error}", ["error": String(text.dropFirst(prefix.count))]) }
        return text
    }

    func removePlaylistImage(id: String) async -> String? {
        guard let core else { return L("Changing a cover needs the server.") }
        let problem = await Self.problem { try await core.removePlaylistImage(id: id) }
        refreshMemo("playlists")
        scheduleBump()
        return problem
    }

    private static func problem(_ change: () async throws -> PlaylistImageResult) async -> String? {
        do { return try await change().problem } catch { return "\(error)" }
    }
}

nonisolated enum CoreCreateError: LocalizedError {
    case noServer, failed(String)
    var errorDescription: String? {
        switch self {
        case .noServer: L("Creating a playlist needs the server.")
        case .failed(let message): message
        }
    }
}

nonisolated struct CoreGenre: Sendable {
    let name: String
    let songs: Int
}

nonisolated struct CoreRanked: Sendable {
    let artist: ArtistRow
    let plays: Int
    let ownAlbums: Int

    func similarCount(in memo: [String: Any]) -> Int? { (memo["similar:\(artist.id)"] as? [SongRow])?.count }
}

nonisolated struct CoreArtistInfo: Sendable {
    let biography: String
    let similar: [String]
}

extension DiscographySection {
    nonisolated init(_ section: ReleaseSection) {
        switch section {
        case .albums: self = .albums
        case .singlesAndEps: self = .singlesAndEPs
        case .compilations: self = .compilations
        case .appearsOn: self = .appearsOn
        }
    }
}
