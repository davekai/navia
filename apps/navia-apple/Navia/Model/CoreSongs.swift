import Foundation
import NaviaCoreFFI
import Observation

/// rows loaded on demand off the main thread, so memory doesn't grow with the library.
@MainActor final class RowCache<Row: Sendable> {
    private var rows: [UInt32: (row: Row, used: UInt64)] = [:]
    private var inFlight = Set<UInt32>()
    private var wanted: [UInt32] = []
    private var loading = false
    private var tick: UInt64 = 0
    private let limit = 4096
    private let fetch: @Sendable ([UInt32]) -> [(UInt32, Row)]
    var onLoaded: (() -> Void)?

    init(fetch: @escaping @Sendable ([UInt32]) -> [(UInt32, Row)]) { self.fetch = fetch }

    func row(_ id: UInt32) -> Row? {
        guard let entry = rows[id] else { return nil }
        tick &+= 1
        rows[id]?.used = tick
        return entry.row
    }

    /// Asks for these ids. Fast scrolling only needs the newest requests, so older ones are dropped.
    func want(_ ids: some Sequence<UInt32>) {
        for id in ids where rows[id] == nil && !inFlight.contains(id) {
            inFlight.insert(id)
            wanted.append(id)
        }
        if wanted.count > 512 {
            let dropped = wanted.prefix(wanted.count - 512)
            dropped.forEach { inFlight.remove($0) }
            wanted.removeFirst(dropped.count)
        }
        pump()
    }

    private func pump() {
        guard !loading, !wanted.isEmpty else { return }
        loading = true
        let batch = wanted
        wanted = []
        let fetch = self.fetch
        Task.detached(priority: .userInitiated) { [weak self] in
            let found = fetch(batch)
            await MainActor.run {
                guard let self else { return }
                self.loading = false
                batch.forEach { self.inFlight.remove($0) }
                for (id, row) in found {
                    self.tick &+= 1
                    self.rows[id] = (row, self.tick)
                }
                self.trim()
                self.onLoaded?()
                self.pump()
            }
        }
    }

    private func trim() {
        guard rows.count > limit else { return }
        let keepAfter = rows.values.map(\.used).sorted()[rows.count - limit * 3 / 4]
        rows = rows.filter { $0.value.used >= keepAfter }
    }

    func invalidate() {
        rows.removeAll()
    }
}

/// The whole library narrowed by the Songs filters, addressed by row. Only row ids live in memory, plus a window
/// of rows.
@Observable final class CoreSongsModel: SongsViewModel {
    @ObservationIgnored private let core: NaviaCore?
    @ObservationIgnored private let collection: LocalCollection
    @ObservationIgnored private let cache: RowCache<SongRow>
    @ObservationIgnored private var order: [UInt32] = []
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var firstListReported = false

    private(set) var count = 0
    private(set) var totalCount = 0
    private(set) var revision = 0
    private(set) var isLoading = true
    private var rowsLoaded = 0
    var editRevision: Int { collection.revision &+ rowsLoaded }

    var query = "" { didSet { if query != oldValue { rebuild() } } }
    /// The library's own order, newest first by date added.
    var sort: SongSort = .added { didSet { if sort != oldValue { descending = false; rebuild() } } }
    var descending = false { didSet { if descending != oldValue { rebuild() } } }
    var ownSort: SongSort { .added }
    func canSort(_ sort: SongSort) -> Bool { sort != .genre }
    var filter: SongFilter = .all { didSet { if filter != oldValue { rebuild() } } }
    @ObservationIgnored var filterSettings: [SongFilterSetting] = [] { didSet { rebuild() } }
    var isNarrowed: Bool { !query.isEmpty || filter != .all || !filterSettings.isEmpty }

    /// The current list's row ids, for handing a window of them to the player.
    var rowids: [UInt32] { order }

    init(core: NaviaCore?, collection: LocalCollection, genre: String? = nil) {
        self.core = core
        self.collection = collection
        if let genre { filterSettings = [SongFilterSetting(id: .genre, value: .list(values: [genre]))] }
        cache = RowCache { ids in
            guard let core else { return [] }
            return core.songsByRowid(rowids: ids).map { ($0.rowid ?? 0, SongRow(core: $0)) }
        }
        cache.onLoaded = { [weak self] in self?.rowsLoaded &+= 1 }
        collection.listen { [weak self] in
            guard let self, self.filter == .favorites || self.filterSettings.contains(where: { $0.id == .favorite || $0.id == .downloaded }) else { return }
            self.rebuild()
        }
        rebuild()
    }

    func reload() {
        cache.invalidate()
        rebuild()
    }

    private func rebuild() {
        generation &+= 1
        let generation = generation
        var settings = filterSettings
        if filter == .favorites, !settings.contains(where: { $0.id == .favorite }) { settings.append(SongFilterSetting(id: .favorite, value: .on)) }
        if filter == .downloaded, !settings.contains(where: { $0.id == .downloaded }) { settings.append(SongFilterSetting(id: .downloaded, value: .on)) }
        let (core, sort, query, narrowed) = (core, sort, query, !query.isEmpty || !settings.isEmpty)
        // The core sorts Length, Date Added and Plays largest first, while a header's ascending is smallest first.
        let reversed = sort.column != nil && descending != [.time, .dateAdded, .playCount].contains(sort)
        Task.detached(priority: .userInitiated) { [weak self] in
            guard let core else { return }
            var order = core.songOrderWithFilters(order: Self.orderBy(sort), query: query, filters: settings)
            if reversed { order.reverse() }
            await MainActor.run {
                guard let self, generation == self.generation else { return }
                self.order = order
                self.count = order.count
                self.isLoading = false
                if !narrowed { self.totalCount = order.count }
                self.revision &+= 1
                if !order.isEmpty, !self.firstListReported {
                    self.firstListReported = true
                    AppTimeline.mark("firstList", rows: order.count)
                }
            }
        }
    }

    /// The list's sort in the core's terms, for Save as Smart Playlist.
    var orderBy: SongOrderBy { Self.orderBy(sort) }

    nonisolated private static func orderBy(_ sort: SongSort) -> SongOrderBy {
        switch sort {
        case .order, .liked, .plays, .playCount: .mostPlayed
        case .added, .dateAdded: .added
        case .title: .title
        case .artist: .artist
        case .album: .album
        case .year: .year
        case .time: .duration
        case .genre: .title
        }
    }

    func song(at index: Int) -> SongRow? {
        guard order.indices.contains(index) else { return nil }
        if let row = cache.row(order[index]) { return collection.apply(row) }
        cache.want(order[max(0, index - 16)..<min(order.count, index + 48)])
        return nil
    }

    func toggleFavorite(at index: Int) { if let song = song(at: index) { toggle(song.id) } }
    @ObservationIgnored var toggle: (String) -> Void = { _ in }

    @ObservationIgnored var downloader: (([SongRow], Bool) -> Void)?
    func setDownloaded(at index: Int, _ downloaded: Bool) {
        if let song = song(at: index) { downloader?([song], downloaded) }
    }
    var allDownloaded: Bool { false }
    func setAllDownloaded(_ downloaded: Bool) {}

    /// for lists small enough to hold; the Songs page never calls this.
    func allSongs() -> [SongRow] {
        guard let core else { return [] }
        return core.songsByRowid(rowids: order).map { collection.apply(SongRow(core: $0)) }
    }

    func entireList() -> [SongRow] { allSongs() }
}

@Observable final class CoreAlbumsModel: AlbumsViewModel {
    @ObservationIgnored private let core: NaviaCore?
    @ObservationIgnored private let library: CoreLibraryModel
    @ObservationIgnored private let scope: AlbumScope
    @ObservationIgnored private let cache: RowCache<AlbumRow>
    @ObservationIgnored private var order: [UInt32] = []
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var listSongs: LocalSongsModel?

    private(set) var count = 0
    private(set) var totalSongs = 0
    private(set) var revision = 0
    private var rowsLoaded = 0

    var query = "" { didSet { if query != oldValue { rebuild() } } }
    var sort: AlbumSort { didSet { if sort != oldValue { rebuild() } } }
    var genre: String? = nil { didSet { if genre != oldValue { rebuild() } } }

    init(core: NaviaCore?, library: CoreLibraryModel, scope: AlbumScope) {
        self.core = core
        self.library = library
        self.scope = scope
        sort = .plays
        cache = RowCache { ids in
            guard let core else { return [] }
            return core.albumsByRowid(rowids: ids).map { ($0.rowid ?? 0, AlbumRow(core: $0)) }
        }
        cache.onLoaded = { [weak self] in self?.rowsLoaded &+= 1 }
        rebuild()
    }

    func reload() {
        cache.invalidate()
        rebuild()
    }

    private func rebuild() {
        generation &+= 1
        let generation = generation
        let (core, sort, query, genre, scope) = (core, sort, query, genre, scope)
        Task.detached(priority: .userInitiated) { [weak self] in
            guard let core else { return }
            var rowids: [UInt32]
            var totalSongs: Int
            if case .artist(let id, let section) = scope {
                let wanted = Set((try? await core.artistDiscography(artistId: id))?.sections.first { DiscographySection($0.section) == section }?.rowids ?? [])
                rowids = core.albumOrder(order: Self.orderBy(sort), query: query, genre: genre, artistId: nil).rowids.filter { wanted.contains($0) }
                totalSongs = core.albumsByRowid(rowids: rowids).reduce(0) { $0 + Int($1.songCount ?? 0) }
            } else {
                let list = core.albumOrder(order: Self.orderBy(sort), query: query, genre: genre, artistId: nil)
                rowids = list.rowids
                totalSongs = Int(list.totalSongs)
            }
            await MainActor.run {
                guard let self, generation == self.generation else { return }
                self.order = rowids
                self.count = rowids.count
                self.totalSongs = totalSongs
                self.revision &+= 1
                self.loadSongs()
            }
        }
    }

    func listedAlbumsSongs() -> any SongsViewModel {
        if let listSongs { return listSongs }
        let model = library.localSongs()
        listSongs = model
        loadSongs()
        return model
    }

    private func loadSongs() {
        guard let model = listSongs, let core else { return }
        let (rowids, generation) = (order, generation)
        Task.detached(priority: .userInitiated) { [weak self] in
            let albums = Dictionary(core.albumsByRowid(rowids: rowids).map { ($0.rowid ?? 0, $0.id) }, uniquingKeysWith: { a, _ in a })
            let rows = rowids.compactMap { albums[$0] }.flatMap { core.albumSongs(albumId: $0).map { SongRow(core: $0) } }
            await MainActor.run {
                guard let self, generation == self.generation else { return }
                model.setRows(rows)
            }
        }
    }

    nonisolated private static func orderBy(_ sort: AlbumSort) -> AlbumOrderBy {
        switch sort {
        case .plays: .mostPlayed
        case .recent: .recentlyAdded
        case .name: .name
        case .year: .year
        }
    }

    func album(at index: Int) -> AlbumRow? {
        _ = rowsLoaded
        guard order.indices.contains(index) else { return nil }
        if let row = cache.row(order[index]) { return library.applied(row) }
        cache.want(order[max(0, index - 12)..<min(order.count, index + 36)])
        return nil
    }
}

/// Every artist, most played first, narrowed by the page search and the Artists filters. The search uses the
/// core's index, which folds case, accents and punctuation and matches each word as a prefix.
@Observable final class CoreArtistsModel: ArtistsViewModel {
    private var rows: [ArtistRow] = []
    private var countsByID: [String: ArtistCountsRow] = [:]
    @ObservationIgnored private let core: NaviaCore?
    @ObservationIgnored private let library: CoreLibraryModel
    @ObservationIgnored private var generation = 0
    var count: Int { rows.count }
    var query = "" { didSet { if query != oldValue { rebuild() } } }
    var filterSettings: [SongFilterSetting] = [] { didSet { rebuild() } }

    init(core: NaviaCore?, library: CoreLibraryModel) {
        self.core = core
        self.library = library
        rebuild()
    }

    private func rebuild() {
        generation &+= 1
        let (core, generation) = (core, generation)
        let query = query.trimmingCharacters(in: .whitespaces)
        let filters = filterSettings
        Task.detached(priority: .userInitiated) { [weak self] in
            guard let core else { return }
            let found = filters.isEmpty ? core.rankedArtists(query: query.isEmpty ? nil : query, limit: nil) : core.rankedArtistsWithFilters(query: query.isEmpty ? nil : query, filters: filters)
            let list = found.map { ArtistRow(core: $0.artist) }
            let needCounts = await MainActor.run { self?.countsByID.isEmpty ?? false }
            let counts = needCounts ? core.artistCounts() : nil
            await MainActor.run {
                if let counts, let self, self.countsByID.isEmpty { self.countsByID = counts }
                guard let self, generation == self.generation else { return }
                self.rows = list
            }
        }
    }

    func artist(at index: Int) -> ArtistRow? { rows.indices.contains(index) ? library.applied(rows[index]) : nil }
    func counts(of id: String) -> ArtistCountsRow? { countsByID[id] }
}

@Observable final class CorePlaylistsModel: PlaylistsViewModel {
    private var all: [PlaylistRow] = []
    private var visible: [PlaylistRow] = []
    private(set) var count = 0
    var query = "" { didSet { if query != oldValue { rebuild() } } }
    var sort: PlaylistSort = .library { didSet { if sort != oldValue { rebuild() } } }
    var kind: PlaylistKind = .all { didSet { if kind != oldValue { rebuild() } } }
    var scope: PlaylistScope = .all { didSet { if scope != oldValue { rebuild() } } }
    var downloadedIDs: Set<String> = [] { didSet { if downloadedIDs != oldValue && scope == .downloaded { rebuild() } } }
    private(set) var hasOthers = false

    init(rows: [PlaylistRow]) { all = rows; rebuild() }

    func setRows(_ rows: [PlaylistRow]) { all = rows; rebuild() }
    var isEmpty: Bool { all.isEmpty }

    private func rebuild() {
        let n = query.trimmingCharacters(in: .whitespaces).lowercased()
        hasOthers = all.contains { $0.section != .mine }
        var found = all.filter { scope.includes($0, downloaded: downloadedIDs) && (kind == .all || $0.isSmart == (kind == .smart)) && (n.isEmpty || $0.name.lowercased().contains(n)) }
        switch sort {
        case .library: break
        case .name: found.sort { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
        case .songs: found.sort { $0.songCount > $1.songCount }
        }
        visible = found
        count = found.count
    }

    func playlist(at index: Int) -> PlaylistRow? { visible.indices.contains(index) ? visible[index] : nil }
}

extension PlaylistScope {
    func includes(_ row: PlaylistRow, downloaded: Set<String>) -> Bool {
        switch self {
        case .all: true
        case .mine: row.section == .mine
        case .shared: row.section == .shared
        case .downloaded: downloaded.contains(row.id)
        }
    }
}
