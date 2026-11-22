import Foundation
import Observation
import NaviaCoreFFI

/// Favorites, downloads and cover choices, shared by every list so they all agree.
@Observable final class LocalCollection {
    private(set) var revision = 0
    private(set) var favoriteSongs: Set<String> = []
    private(set) var downloadedSongs: Set<String> = []
    private(set) var favoriteAlbums: Set<String> = []
    private(set) var favoriteArtists: Set<String> = []
    /// Song ids in like order, newest first. May hold stale ids, so read `likedIDs` instead.
    @ObservationIgnored private var likedOrder: [String] = []
    @ObservationIgnored private var listeners: [() -> Void] = []

    init(favorites: [SongRow], downloaded: [SongRow], favoriteAlbums: Set<String>, favoriteArtists: Set<String>) {
        favoriteSongs = Set(favorites.map(\.id))
        likedOrder = favorites.map(\.id)
        downloadedSongs = Set(downloaded.map(\.id))
        self.favoriteAlbums = favoriteAlbums
        self.favoriteArtists = favoriteArtists
    }

    /// runs after any change, so filtered lists can rebuild. the listener must capture itself weakly.
    func listen(_ action: @escaping () -> Void) { listeners.append(action) }

    /// Replaces the favorites after a server refresh, leaving downloads and covers alone.
    func replaceFavorites(songs: Set<String>, likedOrder: [String], albums: Set<String>, artists: Set<String>) {
        favoriteSongs = songs
        self.likedOrder = likedOrder
        favoriteAlbums = albums
        favoriteArtists = artists
        changed()
    }
    /// after asking the core which files are finished.
    func replaceDownloads(_ ids: Set<String>) {
        guard ids != downloadedSongs else { return }
        downloadedSongs = ids
        changed()
    }

    private func changed() { revision &+= 1; listeners.forEach { $0() } }

    func apply(_ song: SongRow) -> SongRow {
        var out = song
        out.isFavorite = favoriteSongs.contains(song.id)
        out.isDownloaded = downloadedSongs.contains(song.id)
        return out
    }

    /// Rows repaint when a download starts or fails, not only when it finishes.
    func refreshRows() { changed() }

    func toggleFavorite(song id: String) {
        flip(&favoriteSongs, id)
        if favoriteSongs.contains(id) { likedOrder.removeAll { $0 == id }; likedOrder.insert(id, at: 0) }
        changed()
    }

    /// Favorited song ids, newest first. Favorites missing from the order go last.
    var likedIDs: [String] {
        var seen = Set<String>()
        var ids = likedOrder.filter { favoriteSongs.contains($0) && seen.insert($0).inserted }
        ids += favoriteSongs.subtracting(seen).sorted()
        return ids
    }

    func toggleFavorite(album id: String) { flip(&favoriteAlbums, id); changed() }
    func toggleFavorite(artist id: String) { flip(&favoriteArtists, id); changed() }
    func setDownloaded(_ ids: [String], _ on: Bool) {
        for id in ids { if on { downloadedSongs.insert(id) } else { downloadedSongs.remove(id) } }
        changed()
    }

    private func flip(_ set: inout Set<String>, _ id: String) { if set.remove(id) == nil { set.insert(id) } }
}

/// A song list held in memory, for album and playlist pages and for sample mode.
@Observable final class LocalSongsModel: SongsViewModel {
    private var all: [SongRow]
    @ObservationIgnored private var haystacks: [String]
    @ObservationIgnored private var visible: [Int] = []
    @ObservationIgnored private let collection: LocalCollection
    @ObservationIgnored private let defaultSort: SongSort
    @ObservationIgnored private let sortable: Bool
    /// Set on the real core, where a download is a real file. Sample mode only flips the flag.
    @ObservationIgnored var downloader: (([SongRow], Bool) -> Void)?
    /// Set on the real core, where a favorite also goes to the server. Sample mode only toggles the flag.
    @ObservationIgnored var toggle: ((String) -> Void)?
    @ObservationIgnored private var likedFetch: (@Sendable ([String]) -> [SongRow])?
    @ObservationIgnored private var likedShown: [String]?
    @ObservationIgnored private var likedGeneration = 0

    private(set) var count = 0
    private(set) var revision = 0
    private(set) var isLoading = false
    var editRevision: Int { collection.revision }
    var totalCount: Int { all.count }

    var query = "" { didSet { if query != oldValue { rebuild() } } }
    var sort: SongSort { didSet { if sort != oldValue { descending = false; rebuild() } } }
    var descending = false { didSet { if descending != oldValue { rebuild() } } }
    var ownSort: SongSort { defaultSort }
    func canSort(_ sort: SongSort) -> Bool { sortable }
    var filter: SongFilter = .all { didSet { if filter != oldValue { rebuild() } } }

    init(rows: [SongRow], collection: LocalCollection, defaultSort: SongSort = .plays, sortable: Bool = true) {
        all = rows
        self.collection = collection
        self.defaultSort = defaultSort
        self.sortable = sortable
        sort = defaultSort
        haystacks = rows.map { "\($0.title) \($0.artist) \($0.album)".lowercased() }
        rebuild()
        collection.listen { [weak self] in
            guard let self, self.filter != .all else { return }
            self.rebuild() // Only filtered lists change shape when a favorite or download changes.
        }
    }

    /// Keeps this list equal to the favorites, newest first, fetching only rows it doesn't have.
    func followFavorites(fetch: @escaping @Sendable ([String]) -> [SongRow]) {
        likedFetch = fetch
        isLoading = true
        collection.listen { [weak self] in self?.syncFavorites() }
        syncFavorites()
    }

    private func syncFavorites() {
        let want = collection.likedIDs
        guard want != likedShown, let fetch = likedFetch else { return }
        likedShown = want
        likedGeneration &+= 1
        let generation = likedGeneration
        let known = Dictionary(all.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        let missing = want.filter { known[$0] == nil }
        guard !missing.isEmpty else {
            setRows(want.compactMap { known[$0] })
            isLoading = false
            return
        }
        Task.detached(priority: .userInitiated) { [weak self] in
            let found = fetch(missing)
            await MainActor.run {
                guard let self, generation == self.likedGeneration else { return }
                let fetched = Dictionary(found.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
                self.setRows(want.compactMap { known[$0] ?? fetched[$0] })
                self.isLoading = false
            }
        }
    }

    /// the whole list in its own order, which is when songs can be moved.
    var showsListOrder: Bool { sort == .order && filter == .all && query.trimmingCharacters(in: .whitespaces).isEmpty }

    /// Moves songs, by index in the list's own order, to before `before`. Only in list order, unfiltered.
    func move(_ rows: [Int], before: Int) {
        guard showsListOrder else { return }
        let order = movedOrder(len: UInt64(all.count), positions: rows.map { UInt64($0) }, before: UInt64(before)).map { Int($0) }
        guard !order.isEmpty else { return }
        setRows(order.map { all[$0] })
    }

    /// once they've loaded in the background.
    func setRows(_ rows: [SongRow]) {
        all = rows
        haystacks = rows.map { "\($0.title) \($0.artist) \($0.album)".lowercased() }
        rebuild()
    }

    private func rebuild() {
        let needle = query.trimmingCharacters(in: .whitespaces).lowercased()
        var indices = needle.isEmpty ? Array(all.indices) : all.indices.filter { haystacks[$0].contains(needle) }
        if filter == .favorites {
            indices = indices.filter { collection.favoriteSongs.contains(all[$0].id) }
        } else if filter == .downloaded {
            indices = indices.filter { collection.downloadedSongs.contains(all[$0].id) }
        }
        let rows = all
        func text(_ a: String, _ b: String) -> Bool { a.localizedStandardCompare(b) == .orderedAscending }
        switch sort {
        // Local rows carry no date, so Date Added keeps their order.
        case .order, .liked, .added: break
        case .plays: indices.sort { rows[$0].plays > rows[$1].plays }
        case .title: indices.sort { text(rows[$0].title, rows[$1].title) }
        case .artist: indices.sort { text(rows[$0].artist, rows[$1].artist) }
        case .album: indices.sort { text(rows[$0].album, rows[$1].album) }
        case .year: indices.sort { (rows[$0].year ?? 0) > (rows[$1].year ?? 0) }
        case .time: indices.sort { rows[$0].duration < rows[$1].duration }
        case .genre: indices.sort { text(rows[$0].genre, rows[$1].genre) }
        // Rows without a date keep their order among themselves.
        case .dateAdded: indices.sort { (rows[$0].added ?? "") < (rows[$1].added ?? "") }
        case .playCount: indices.sort { rows[$0].plays < rows[$1].plays }
        }
        if descending, sort.column != nil { indices.reverse() }
        visible = indices
        count = indices.count
        revision &+= 1
    }

    func song(at index: Int) -> SongRow? {
        visible.indices.contains(index) ? collection.apply(all[visible[index]]) : nil
    }

    func toggleFavorite(at index: Int) {
        guard visible.indices.contains(index) else { return }
        let id = all[visible[index]].id
        if let toggle { toggle(id) } else { collection.toggleFavorite(song: id) }
    }

    func setDownloaded(at index: Int, _ downloaded: Bool) {
        guard visible.indices.contains(index) else { return }
        if let downloader { downloader([collection.apply(all[visible[index]])], downloaded) } else { collection.setDownloaded([all[visible[index]].id], downloaded) }
    }

    func allSongs() -> [SongRow] { visible.map { collection.apply(all[$0]) } }
    func entireList() -> [SongRow] { all.map { collection.apply($0) } }
    /// The list position of shown song `index`, whatever the sort and filter.
    func listPlace(of index: Int) -> Int? { visible.indices.contains(index) ? visible[index] : nil }

    var allDownloaded: Bool {
        !all.isEmpty && all.allSatisfy { collection.downloadedSongs.contains($0.id) }
    }

    func setAllDownloaded(_ downloaded: Bool) {
        if let downloader { downloader(all.map { collection.apply($0) }, downloaded) } else { collection.setDownloaded(all.map(\.id), downloaded) }
    }
}
