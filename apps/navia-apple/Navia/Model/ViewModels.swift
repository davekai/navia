import Foundation
import Observation
import NaviaCoreFFI

// What the interface needs from the core. Sample data fills these when there's no core.
// Long lists are fetched by index so the core can page through the library.

nonisolated struct SongRow: Identifiable, Equatable, Sendable {
    let id: String
    let title: String
    let artist: String
    let album: String
    let albumID: String
    let duration: Int
    let genre: String
    let year: Int?
    let plays: Int
    var isFavorite: Bool
    var isDownloaded: Bool
    let artworkURL: URL?
    var tintHex: String?
    var trackNumber: Int? = nil
    var hasLyrics = false
    var isHiRes = false
    /// The local database row, when it came from the core. Lets the player queue large lists cheaply.
    var rowid: UInt32? = nil
    /// Every credited artist. Empty in sample data.
    var credits: [FeaturedArtist] = []
    var disc: Int? = nil
    /// OpenSubsonic `explicitStatus`.
    var isExplicit = false
    /// When it was added to the server, as ISO 8601.
    var added: String? = nil
}

nonisolated struct FeaturedArtist: Identifiable, Equatable, Sendable {
    var id: String { name }
    let name: String
    /// The artist's own photo. Nil shows the plain fallback, never an album cover.
    let photoURL: URL?
    /// Nil when the server has no page for this credit.
    var artistID: String? = nil
}

nonisolated struct AlbumRow: Identifiable, Equatable, Sendable {
    let id: String
    let name: String
    let artist: String
    let artistID: String?
    let year: Int?
    let songCount: Int
    let duration: Int
    let genre: String
    let artworkURL: URL?
    var tintHex: String?
    /// Plain text from the server's agents. Empty when it has none.
    var blurb: String
    var isFavorite: Bool
    var isExplicit = false
    /// Disc names by disc number, only where the server has one (OpenSubsonic `discTitles`).
    var discTitles: [Int: String] = [:]
    /// Every credited album artist with a page, in credit order. Empty in sample data.
    var credits: [FeaturedArtist] = []
    /// The album's version as a label, such as "Deluxe Edition". Nil for none, and for explicit or clean.
    var edition: String? = nil
    /// by the first-release date, so a reissue isn't new.
    var isNewRelease = false
}

/// the core's `UpcomingAlbumRow`.
nonisolated struct UpcomingAlbum: Identifiable, Equatable, Sendable {
    /// Deezer's album id.
    let id: UInt64
    let title: String
    let artist: String
    /// the library artist it was found for.
    let artistID: String
    /// never stored (Deezer's terms).
    let coverURL: URL?
    /// `YYYY-MM-DD`.
    let releaseDate: String
    /// local midnight of the release day.
    let releaseMoment: Date
    /// songs announced in all; `tracks` are those named so far, filled only on the page.
    let total: Int
    let isExplicit: Bool
    /// Deezer's listing where the library has no copy; its id is in `unavailable`.
    var tracks: [SongRow] = []
    var unavailable: Set<String> = []
}

nonisolated enum UpcomingPageState: Equatable, Sendable {
    case loading
    case missing
    case upcoming(UpcomingAlbum)
    /// the server has it now: its library album's id.
    case onServer(String)
}

nonisolated extension Array where Element == AlbumRow {
    /// Lowercased names that more than one album here has, so their cards show the edition.
    var sharedNames: Set<String> {
        var counts: [String: Int] = [:]
        for album in self { counts[album.name.lowercased(), default: 0] += 1 }
        return Set(counts.filter { $0.value > 1 }.keys)
    }
}

nonisolated struct ArtistRow: Identifiable, Equatable, Sendable {
    let id: String
    let name: String
    let photoURL: URL?
    let tintHex: String?
    let bio: String
    var isFavorite: Bool
}

/// Why a smart playlist wasn't saved, in words to show.
nonisolated struct SmartSaveFailure: Error, Sendable {
    let message: String
}

nonisolated struct RuleSummary: Equatable, Sendable {
    /// The whole playlist in one line. Empty for sample summaries.
    var sentence = ""
    let match: String
    let rules: [String]
    let order: [String]
}


nonisolated struct PlaylistRow: Identifiable, Equatable, Sendable {
    let id: String
    let name: String
    let songCount: Int
    let duration: Int
    let summary: String
    let isSmart: Bool
    let isPublic: Bool
    let rules: RuleSummary?
    /// Only the owner can update a playlist, so Edit, Rules and Public appear only for these.
    var isOwned = false
    /// Shown as "By <name>" on other people's playlists.
    var ownerName = ""
    /// The signed-in user owns it, or the owner is unknown. Being an admin doesn't change this.
    var isMine = true
    var section: PlaylistSection { PlaylistSection(isMine: isMine, isPublic: isPublic) }
    /// The server's cover (an upload or its own grid), smart playlists included.
    var coverURL: URL? = nil
    /// Up to four album covers, for a playlist the server has no cover for.
    var collage: [URL] = []
    /// A regular playlist the server doesn't mark read-only.
    var canReorder = false
}

/// The result of changing a playlist's songs. Anything but `saved` carries a message to show.
nonisolated enum PlaylistEdit: Sendable, Equatable {
    case saved
    /// The playlist changed on the server and its stored copy is fresh, so the page reloads it.
    case changed(String)
    case failed(String)
}

/// Where a playlist is listed, by who owns it.
nonisolated enum PlaylistSection: Sendable {
    /// Yours, private or public, or with an unknown owner.
    case mine
    /// Someone else's public playlist.
    case shared
    /// Someone else's private playlist, which only an admin sees. Listed only under All.
    case other

    init(isMine: Bool, isPublic: Bool) { self = isMine ? .mine : isPublic ? .shared : .other }

    /// Navidrome compares usernames case-insensitively. An unknown owner counts as yours.
    static func owns(_ owner: String?, me: String) -> Bool {
        guard let owner, !owner.isEmpty else { return true }
        return owner.lowercased() == me.lowercased()
    }
}

nonisolated struct PlaylistRef: Identifiable, Equatable, Sendable {
    let id: String
    let name: String
    var isMine = true
    var isPublic = false
    var section: PlaylistSection { PlaylistSection(isMine: isMine, isPublic: isPublic) }
    var coverURL: URL? = nil
    var isSmart = false
    var collage: [URL] = []
}

nonisolated struct LyricLine: Equatable, Sendable {
    /// Milliseconds from the start of the song. Nil when the server's lyrics are unsynced.
    let start: Int?
    let text: String
    /// The exact end, when the server has one (`songLyrics` v2).
    var endMs: Int? = nil
    /// Word timing from the server (`songLyrics` v2). Empty means the fill is estimated.
    var words: [LyricWord] = []
}

extension [LyricLine] {
    /// The last synced line that has started at `ms`, or the first before any has.
    nonisolated func activeIndex(atMs ms: Int) -> Int { lastIndex { ($0.start ?? 0) <= ms } ?? 0 }
}

extension PlayerViewModel {
    /// the current song has lyrics, or it isn't known yet because they're still being asked for.
    var mayHaveLyrics: Bool { current?.hasLyrics == true || lyricsPending }
}

extension PlayerViewModel {
    /// whether `lines` are really timed, since some tags give every line a start of 0.
    func lyricsAreSynced(_ lines: [LyricLine]) -> Bool {
        guard lyricsSynced, lines.allSatisfy({ $0.start != nil }) else { return false }
        return Set(lines.compactMap(\.start)).count > 1
    }
}

/// One timed word or syllable, as a UTF-8 byte range of its line, end exclusive.
nonisolated struct LyricWord: Equatable, Sendable {
    let start: UInt32
    let end: UInt32
    let startMs: UInt32
    let endMs: UInt32?
}

/// A downloaded song and its real size on disk.
nonisolated struct DownloadEntry: Sendable {
    let song: SongRow
    let bytes: Int64
}

nonisolated struct RadioStation: Identifiable, Equatable, Sendable {
    var id: String { url }
    let name: String
    let url: String
}

nonisolated enum SongSort: String, CaseIterable, Identifiable, Sendable {
    case order, liked, plays, added, title, artist, album, year, time, genre
    /// As column headers these sort ascending, oldest and fewest first. The sort menus use newest and most first.
    case dateAdded, playCount
    var id: String { rawValue }
    var label: String {
        switch self {
        case .order: L("Playlist Order")
        case .liked: L("Recently Liked")
        case .plays: L("Most Played")
        case .added: L("Date Added")
        case .title: L("Title")
        case .artist: L("Artist")
        case .album: L("Album")
        case .year: L("Year")
        case .time: L("Length")
        case .genre: L("Genre")
        case .dateAdded: L("Date Added")
        case .playCount: L("Plays")
        }
    }

    /// The sort for a column header, by position: 0 Song Name, 1 Time, 2 Artist, 3 Album, 4 Genre, 5 Date Added, 6 Plays.
    static func column(_ index: Int) -> SongSort? {
        switch index {
        case 0: .title
        case 1: .time
        case 2: .artist
        case 3: .album
        case 4: .genre
        case 5: .dateAdded
        case 6: .playCount
        default: nil
        }
    }

    var column: Int? {
        switch self {
        case .title: 0
        case .time: 1
        case .artist: 2
        case .album: 3
        case .genre: 4
        case .dateAdded: 5
        case .playCount: 6
        default: nil
        }
    }
    /// What the sort menu offers: the list's own order, then the column sorts, then the rest.
    static let librarySorts: [SongSort] = [.added, .title, .time, .artist, .album, .plays, .year]
    static let playlistSorts: [SongSort] = [.order, .title, .time, .artist, .album]
    static let likedSorts: [SongSort] = [.liked, .title, .time, .artist, .album]
}

nonisolated enum SongFilter: String, CaseIterable, Identifiable, Sendable {
    case all, favorites, downloaded
    var id: String { rawValue }
    var label: String { switch self { case .all: L("All Songs"); case .favorites: L("Favorites"); case .downloaded: L("Downloaded") } }
}

protocol SongsViewModel: AnyObject, Observable {
    /// Rows after the current search and filter.
    var count: Int { get }
    /// Rows in the whole list, ignoring the search and filter.
    var totalCount: Int { get }
    /// Changes when rows are added, removed or reordered. Edits to one row don't change it, so a table
    /// can reload just that row and keep its selection.
    var revision: Int { get }
    /// Changes when a favorite or download flag changes anywhere, so visible rows can repaint in place.
    var editRevision: Int { get }
    var query: String { get set }
    /// Setting a different sort puts it back to ascending.
    var sort: SongSort { get set }
    /// A column sort reversed. Ascending is A to Z, and shortest first for Length.
    var descending: Bool { get set }
    /// The list's own order, such as playlist order or date liked. A third header click returns to it.
    var ownSort: SongSort { get }
    var filter: SongFilter { get set }
    func song(at index: Int) -> SongRow?
    func toggleFavorite(at index: Int)
    func setDownloaded(at index: Int, _ downloaded: Bool)
    /// Every row in the current order, for handing the whole list to the player.
    func allSongs() -> [SongRow]
    /// The whole list ignoring search and filter, in its natural order.
    func entireList() -> [SongRow]
    var allDownloaded: Bool { get }
    func setAllDownloaded(_ downloaded: Bool)
    /// True until the first answer for this list arrives, so an empty list isn't mistaken for no songs.
    var isLoading: Bool { get }
    func canSort(_ sort: SongSort) -> Bool
}

extension SongsViewModel {
    var isLoading: Bool { false }
    /// Whether this list can be sorted that way. The library's own song list has no genre order.
    func canSort(_ sort: SongSort) -> Bool { true }

    /// A click on a column header: ascending, then descending, then the list's own order.
    func headerClicked(_ column: Int) {
        guard let next = SongSort.column(column), canSort(next) else { return }
        if sort != next { sort = next; descending = false }
        else if !descending { descending = true }
        else { sort = ownSort; descending = false }
    }
}

nonisolated enum AlbumScope: Hashable, Sendable {
    case all
    /// One section of an artist's discography.
    case artist(String, DiscographySection)
}

/// The artist page's album sections, in page order.
nonisolated enum DiscographySection: Hashable, Sendable, CaseIterable {
    case albums, singlesAndEPs, compilations, appearsOn

    var title: String {
        switch self {
        case .albums: L("Albums")
        case .singlesAndEPs: L("Singles & EPs")
        case .compilations: L("Compilations")
        case .appearsOn: L("Appears On")
        }
    }
}

/// One section of an artist's albums, newest first.
nonisolated struct DiscographyPart: Sendable, Identifiable {
    let section: DiscographySection
    let albums: [AlbumRow]
    var id: DiscographySection { section }
}

/// "More by <album artist>" under an album.
nonisolated struct MoreBy: Sendable {
    let artist: ArtistRow
    let albums: [AlbumRow]
    let section: DiscographySection
}

nonisolated enum AlbumSort: String, CaseIterable, Identifiable, Sendable {
    /// `recent` is newest first by date added.
    case plays, recent, name, year
    var id: String { rawValue }
    var label: String {
        switch self {
        case .plays: L("Most Played")
        case .recent: L("Date Added")
        case .name: L("Name")
        case .year: L("Year")
        }
    }
}

protocol AlbumsViewModel: AnyObject, Observable {
    var count: Int { get }
    var totalSongs: Int { get }
    var revision: Int { get }
    var query: String { get set }
    var sort: AlbumSort { get set }
    var genre: String? { get set }
    func album(at index: Int) -> AlbumRow?
    func listedAlbumsSongs() -> any SongsViewModel
}

protocol ArtistsViewModel: AnyObject, Observable {
    /// The page search. Empty shows every artist.
    var query: String { get set }
    /// The Artists filters that are set, applied with the search.
    var filterSettings: [SongFilterSetting] { get set }
    var count: Int { get }
    func artist(at index: Int) -> ArtistRow?
    /// The artist's album and song counts. Nil until read, and in sample mode.
    func counts(of id: String) -> ArtistCountsRow?
}

nonisolated enum PlaylistSort: String, CaseIterable, Identifiable, Sendable {
    case library, name, songs
    var id: String { rawValue }
    var label: String { switch self { case .library: L("Library Order"); case .name: L("Name"); case .songs: L("Song Count") } }
}

nonisolated enum PlaylistScope: String, CaseIterable, Identifiable, Sendable {
    case all, mine, shared, downloaded
    var id: String { rawValue }
    var label: String {
        switch self {
        case .all: L("All||playlists")
        case .mine: L("Mine||playlists")
        case .shared: L("Shared||playlists")
        case .downloaded: L("Downloaded||playlists")
        }
    }
}

nonisolated enum PlaylistKind: String, CaseIterable, Identifiable, Sendable {
    case all, smart, regular
    var id: String { rawValue }
    var label: String { switch self { case .all: L("All Playlists"); case .smart: L("Smart"); case .regular: L("Regular") } }
}

protocol PlaylistsViewModel: AnyObject, Observable {
    var count: Int { get }
    var query: String { get set }
    var sort: PlaylistSort { get set }
    var kind: PlaylistKind { get set }
    var scope: PlaylistScope { get set }
    /// Playlists with Download on.
    var downloadedIDs: Set<String> { get set }
    /// Some playlist isn't the user's own, so Mine and Shared mean something.
    var hasOthers: Bool { get }
    func playlist(at index: Int) -> PlaylistRow?
}

nonisolated enum SearchSort: String, CaseIterable, Identifiable, Sendable {
    case relevance, name, year
    var id: String { rawValue }
    var label: String { rawValue.capitalized }
}

nonisolated struct SearchResults: Sendable {
    let topArtist: ArtistRow?
    let topArtistSongs: [SongRow]
    let artists: [ArtistRow]
    let albums: [AlbumRow]
    let songs: [SongRow]
}

/// Whether the library can be shown. Empty pages say why not, so "no results" never stands in for "not ready".
nonisolated enum LibraryState: Equatable, Sendable {
    /// The local database hasn't answered yet.
    case opening
    /// No login to connect with.
    case noServer
    /// Connected, with the first sync still running.
    case syncing
    /// The server couldn't be reached and there is nothing local to show.
    case unreachable
    /// The sync stopped partway with nothing to show.
    case syncFailed
    /// The local library is usable. It may really be empty.
    case ready
}

extension LibraryViewModel {
    func onRepeat() -> [SongRow] { [] }
    func backInRotation() -> [ArtistRow] { [] }
    func becauseYouPlayed() -> (seed: ArtistRow, artists: [ArtistRow])? { nil }
    /// Home's Artist Radio row. Empty until worked out, and without similar-artist data.
    func artistStations() -> [ArtistStation] { [] }
    func similarArtists(for artist: ArtistRow) -> [ArtistRow] { [] }
    func moreBy(album: AlbumRow) -> MoreBy? { nil }
    func albumHeat(album: AlbumRow) -> [String: UInt64] { [:] }
    func reaskMissingHeat(album: AlbumRow) {}
    func upcoming(for artist: ArtistRow) -> [UpcomingAlbum] { [] }
    func upcomingHome() -> [UpcomingAlbum] { [] }
    func upcomingMatching(_ query: String) -> [UpcomingAlbum] { [] }
    func upcomingPage(_ id: UInt64) -> UpcomingPageState { .missing }
    func playsOffline(song: SongRow) -> Bool { isDownloaded(song: song) }
    var storage: StorageSettings? { nil }
    var playThreshold: PlayThresholdSettings? { nil }
}

protocol LibraryViewModel: AnyObject, Observable {
    /// The running sync to show, if any.
    var syncProgress: SyncProgressRow? { get }
    /// An empty page is showing the sync meter, so the sidebar's card waits.
    var syncMeterOnPage: Bool { get }
    func syncMeterVisible(_ on: Bool)
    /// Share links. None without a server.
    var shareLinks: ShareLinks? { get }
    var state: LibraryState { get }
    /// True when the server can't be reached. Local browsing and downloads keep working.
    var isOffline: Bool { get }
    /// Tries the server now. True when it answered.
    func retryConnection() async -> Bool
    /// a page's refresh: brings in what changed on the server since the last look and reloads the rows from it,
    /// keeping the old ones on screen meanwhile. does nothing while one runs.
    func refreshNow()
    /// whether a page's refresh runs, or with `library` also any library sync.
    func isRefreshing(library: Bool) -> Bool
    /// the account menu's Offline switch, kept per account: on, nothing goes to the server.
    var offlineSwitch: Bool { get }
    /// off reconnects at once, the way Retry does.
    func setOfflineSwitch(_ on: Bool)
    var allSongs: any SongsViewModel { get }
    var playlists: [PlaylistRef] { get }
    /// Keeps this account's device-local data apart from another's.
    var accountKey: String { get }
    var genres: [String] { get }
    /// The Genres page's tiles and rows, plus Search's Browse by Genre tiles. Empty until loaded.
    var genrePage: GenrePage { get }

    func album(id: String) -> AlbumRow?
    func artist(id: String) -> ArtistRow?
    func artist(named name: String) -> ArtistRow?
    func playlist(id: String) -> PlaylistRow?

    func albums(scope: AlbumScope) -> any AlbumsViewModel
    func artists() -> any ArtistsViewModel
    func playlistsList() -> any PlaylistsViewModel
    func songs(forAlbum id: String) -> any SongsViewModel
    func songs(forPlaylist id: String) -> any SongsViewModel
    func songs(forGenre genre: String) -> any SongsViewModel
    /// This user's favorited songs, newest first.
    func likedSongs() -> any SongsViewModel

    func essentialsArtists(limit: Int) -> [ArtistRow]
    func essentialsSongs(for artist: ArtistRow) -> [SongRow]
    func worldOf() -> (artist: ArtistRow, albums: [AlbumRow])?
    func recentAlbums(limit: Int) -> [AlbumRow]
    func topAlbums(limit: Int) -> [AlbumRow]
    /// Discover's For You. Nil while it's being worked out.
    func forYou() -> ForYouRows?
    /// One of today's mixes. Nil once the day has changed or before For You has loaded.
    func mix(id: String) -> MixInfo?
    func songs(forMix mix: MixInfo) -> any SongsViewModel
    /// in the server's order, which the table can't change.
    func songs(forArtistTop rows: [SongRow]) -> any SongsViewModel
    func latestSongs(limit: Int) -> [SongRow]
    func topSongs(limit: Int) -> [SongRow]
    func topArtists(limit: Int) -> [ArtistRow]
    /// Home rows that need dated plays. Empty or nil where the server keeps no play history.
    func onRepeat() -> [SongRow]
    func backInRotation() -> [ArtistRow]
    func becauseYouPlayed() -> (seed: ArtistRow, artists: [ArtistRow])?
    /// Home's Artist Radio row, worked out off the main thread.
    func artistStations() -> [ArtistStation]
    var stations: [RadioStation] { get }
    /// The server this app is connected to, or a stand-in for sample data.
    var serverLabel: String { get }
    /// True when play reports reach the server. Favorites stay local while this is off.
    var reportsPlays: Bool { get }
    func setReporting(_ on: Bool)
    /// Streaming, metered and download quality, and volume leveling.
    var quality: QualitySettings? { get }
    func setBackgroundSync(_ on: Bool)
    var downloadedCount: Int { get }
    /// Every download with its size on disk.
    func downloadEntries() async -> [DownloadEntry]
    /// Synced playlists and albums. Nil without the core.
    var synced: SyncedDownloads? { get }
    func radioArtists(limit: Int) -> [ArtistRow]

    func topSongs(for artist: ArtistRow) -> [SongRow]
    /// Top songs, or the artist's own album songs when the server has no top songs for them.
    func playableSongs(for artist: ArtistRow) -> [SongRow]
    func albums(for artist: ArtistRow) -> [AlbumRow]
    /// The artist's albums by release type, empty sections left out. Nil while it's being worked out.
    func discography(for artist: ArtistRow) -> [DiscographyPart]?
    func similarSongs(for artist: ArtistRow) -> [SongRow]
    /// The artist page's Similar Artists, in order. Empty until the answer lands, and when there's none.
    func similarArtists(for artist: ArtistRow) -> [ArtistRow]
    /// "More by <album artist>" under an album, or nil to hide it.
    func moreBy(album: AlbumRow) -> MoreBy?
    /// the album's Heat column: Deezer's `rank` by song id, only with the Deezer switch on; what's kept shows until a
    /// fresh answer lands, and songs Deezer has no match for are left out.
    func albumHeat(album: AlbumRow) -> [String: UInt64]
    /// an empty answer (offline, or no match) is asked again each time the album opens, not kept for the session
    func reaskMissingHeat(album: AlbumRow)
    /// empty until the answer lands.
    func upcoming(for artist: ArtistRow) -> [UpcomingAlbum]
    func upcomingHome() -> [UpcomingAlbum]
    /// never asks Deezer.
    func upcomingMatching(_ query: String) -> [UpcomingAlbum]
    func upcomingPage(_ id: UInt64) -> UpcomingPageState
    func songs(forUpcoming album: UpcomingAlbum) -> any SongsViewModel
    func songs(by artist: ArtistRow, matching query: String) -> [SongRow]
    /// A queue for Song Radio from one song. Nil when the server has no similar-artist data for it.
    func radioQueue(for song: SongRow) -> [SongRow]?
    /// Asks the server for similar songs for one song, then that song first. Nil when it has none.
    func radioQueue(startingFrom song: SongRow) async -> [SongRow]?
    /// A radio from one song, without the song itself. Empty when there's none.
    func radioSongs(after song: SongRow) async -> [SongRow]
    /// Artist Radio, from Deezer while the Deezer switch is on, filled with the server's similar songs.
    /// Empty when there's none.
    func artistRadio(for artist: ArtistRow) async -> [SongRow]
    /// True once the server has said it has no radio for this song.
    func knownNoRadio(_ song: SongRow) -> Bool
    /// Gives the next batch of Recommended songs for a playlist on each call, empty when there's nothing left.
    /// Nil without a server.
    func recommender(forPlaylist id: String) -> (() async -> [SongRow])?

    /// Searches off the main thread.
    func searchResults(_ query: String, sort: SearchSort, favoritesOnly: Bool) async -> SearchResults

    /// A smart playlist's rules as Navidrome's JSON. Nil when they aren't known here yet.
    func smartRulesJSON(playlistID: String) -> String?
    /// Runs the editor's rules over the local library.
    func smartPreview(editor: NaviaSmartEditor, maxRows: Int) async -> (preview: SmartPreviewResult, songs: [SongRow])
    /// Why this name and these rules can't be saved, or nil when they can. Checks the form only.
    func smartRulesProblem(name: String, criteria: String) -> String?
    /// Saves a smart playlist, creating it when `id` is nil. Returns its id, or the message to show on failure.
    func saveSmartPlaylist(id: String?, name: String, criteria: String) async -> Result<String, SmartSaveFailure>

    /// Changes when any favorite or download changes.
    var collectionRevision: Int { get }
    func toggleFavorite(album id: String)
    func toggleFavorite(artist id: String)
    func toggleFavorite(song: SongRow)
    func isDownloaded(song: SongRow) -> Bool
    func isFavorite(song: SongRow) -> Bool
    /// Plays without the server: downloaded, or in the song cache.
    func playsOffline(song: SongRow) -> Bool
    /// Settings > Storage. Nil without a core.
    var storage: StorageSettings? { get }
    /// When a play counts for scrobbling. Nil without a core.
    var playThreshold: PlayThresholdSettings? { get }
    /// Fraction 0 to 1 of a download in flight, or nil.
    func downloadProgress(song: SongRow) -> Double?
    func isDownloading(song: SongRow) -> Bool
    func setDownloaded(songs: [SongRow], _ downloaded: Bool)
    /// Saves a playlist's new song order. `expected` is the order that was shown.
    func savePlaylistOrder(id: String, expected: [String], order: [String]) async -> PlaylistEdit
    /// The playlist's songs as stored, after a refused save brought in the server's copy.
    func storedPlaylistSongs(id: String) async -> [SongRow]?
    /// Creates a regular playlist with these songs. Returns its id when the server gives one.
    func createPlaylist(name: String, songs: [SongRow]) async throws -> String?
    /// The user's own regular playlists, most recently changed first.
    func playlistTargets() -> [PlaylistRef]
    func addToPlaylist(id: String, songs: [SongRow]) async -> PlaylistEdit
    /// Which of these songs the playlist already has, so an add can warn about duplicates.
    func alreadyInPlaylist(id: String, songs: [SongRow]) -> Set<String>
    /// Removes the songs at these positions. Nothing is removed if the server's copy differs from `expected`.
    func removeFromPlaylist(id: String, expected: [String], places: [Int]) async -> PlaylistEdit
    /// Whether the server lets this user upload and remove playlist images.
    var canChangePlaylistImages: Bool { get }
    /// Uploads this image file as the playlist's cover. Returns an error message, or nil.
    func setPlaylistImage(id: String, path: String) async -> String?
    /// Removes the playlist's uploaded image. Returns an error message, or nil.
    func removePlaylistImage(id: String) async -> String?
    /// Changes a playlist's name, description or Public setting. Nil leaves a field as it is, and an empty
    /// description clears it. Returns an error message, or nil.
    func updatePlaylistDetails(id: String, name: String?, comment: String?, isPublic: Bool?) async -> String?
    /// A playlist's songs as stored on this device, read now (a playlist's menu). empty until the playlist was fetched.
    func playlistSongsNow(id: String) -> [SongRow]
    /// A playlist's songs to play from its menu: the stored ones, fetched first when none are stored yet.
    func playlistSongsToPlay(id: String) async -> [SongRow]
    /// deletes a playlist the user owns, on the server for everyone. returns the words to show when it didn't go, or nil.
    func deletePlaylist(id: String) async -> String?
}

/// One position in the queue. The same song can appear twice, so entries have their own identity.
nonisolated struct QueueEntry: Identifiable, Equatable, Sendable {
    let id: Int
    let song: SongRow
    /// Added with Play Next or Add to Queue, so it plays before the rest of the list.
    var queued = false
    /// Added by Autoplay.
    var auto = false
}

/// What the queue plays from, for its "Next from" heading, and the page its name links to.
struct PlaySource: Equatable {
    var name: String
    var radio = false
    var link: Route?

    var title: String { radio ? L("{name} Radio", ["name": name]) : name }
}

/// What the queue's Autoplay section shows.
enum AutoplayShown: Sendable {
    case on
    /// The list is over and songs are being found. Shows the heading with placeholder rows.
    case finding
    /// Turned off in Settings, or nothing to offer.
    case hidden
    /// Repeat is on. Shows a dimmed heading with a note.
    case repeating
}

protocol PlayerViewModel: AnyObject, Observable {
    /// The current song's quality. Nil for a station or when the server didn't say.
    var quality: AudioQuality? { get }
    var current: SongRow? { get }
    var featuredArtists: [FeaturedArtist] { get }
    /// Synced lines for the current song. Empty when the server has none or they haven't arrived.
    var lyrics: [LyricLine] { get }
    /// The server's flag for whether the lines are timed. Views also check the times themselves.
    var lyricsSynced: Bool { get }
    /// the current song's lyrics are still being asked for, so it isn't known yet whether it has any.
    var lyricsPending: Bool { get }
    var isPlaying: Bool { get }
    /// Seconds into the current song.
    var position: Double { get }
    /// 0 to 100. Muting keeps this value so it can come back.
    var volume: Double { get set }
    var isMuted: Bool { get set }
    var shuffle: Bool { get set }
    /// Off, all or one.
    var repeatMode: PlayerRepeat { get set }
    /// Plays aren't counted or scrobbled while this is on.
    var privateMode: Bool { get set }
    /// The internet radio station playing instead of the queue.
    var station: RadioStation? { get }
    /// Playback ran out of audio and is waiting for the server to come back.
    var isWaitingForServer: Bool { get }
    /// The device that saved a newer queue on the server, while that queue is offered.
    var queueOfferDevice: String? { get }
    /// Accepting loads the offered queue paused at its place. Either way it isn't offered again.
    func answerQueueOffer(accept: Bool)
    func togglePlayPause()
    /// Tries the server now.
    func retryServer()
    func next()
    func previous()
    func seek(to seconds: Double)
    func toggleFavorite()
    func play(songAt index: Int, in songs: any SongsViewModel)
    /// Replaces the queue. `shuffle` overrides the shuffle state, and nil keeps it.
    func play(_ songs: [SongRow], at index: Int, shuffle: Bool?)
    func playNext(_ song: SongRow)
    func addToQueue(_ song: SongRow)
    func playStation(_ station: RadioStation)
    func stopStation()

    /// `smart`: a song that continues the previous one's album in order joins gapless. `mix`: each
    /// join's length, place, beat and loudness come from the songs.
    func setCrossfade(ms: Int, smart: Bool, mix: Bool)
    func setOutputDevice(_ id: String?)
    /// The chain the audio should use, or nil to play unprocessed.
    func applyEqualizer(_ plan: EqPlan?)
    func setDiscordStatus(_ on: Bool)
    /// The public server address Discord should use for covers.
    func setDiscordPublicAddress(_ text: String)
    /// Where Discord covers come from: "auto", "server" or "search".
    func setDiscordCoversFrom(_ value: String)
    /// While false, `position` holds still so views don't redraw for a hidden window. Playback carries on.
    func setViewsLive(_ live: Bool)

    /// Entries after the current one.
    var upcomingCount: Int { get }
    func upcoming(at offset: Int) -> QueueEntry?
    func jump(toUpcoming offset: Int)
    /// Moves upcoming entries, in order, to before the entry now at `before`. `upcomingCount` means the end.
    /// The playing song never moves.
    func moveUpcoming(_ offsets: [Int], before: Int)
    /// Plays one song now, then its radio as the rest of the list, unless something else was played meanwhile.
    /// Shuffle turns off.
    func playAsRadio(_ song: SongRow, radio: @escaping @MainActor () async -> [SongRow])

    /// Per account, on by default.
    var autoplayEnabled: Bool { get set }
    /// Navia's engine, the default, or the server's.
    var autoplayMode: AutoplayModeRow { get set }
    /// Fewer Repeats, the default, or Standard.
    var shuffleStyle: ShuffleStyle { get set }
    /// Settings > Playback > Match the song's sample rate, on by default.
    var matchSampleRate: Bool { get set }
    /// Settings > Share what I'm playing, off unless the listener opts in.
    var shareListening: Bool { get set }
    /// Listening Now. Nil in sample mode.
    var listen: ListenAlongModel? { get }
    /// The user being followed, whose song plays in place of the queue.
    var followingUser: String? { get }
    /// The Discord id of the Spotify friend being followed, if any.
    var followingFriend: String? { get }
    /// The followed Spotify friend's song isn't in the library, so nothing plays until one is.
    var followingWaiting: Bool { get }
    /// The listener paused while following. It stays paused and following.
    var followingHeld: Bool { get }
    var followingSending: Bool { get }
    /// a listening session is on: Next in Queue is the session's.
    var sessionOn: Bool { get }
    /// who on the other side has songs queued for the session.
    var sessionTheirs: [String] { get }
    /// Ends following quietly (the chip's Stop, the row's headphones).
    func stopFollowing()
    /// The listener's own song, which carries on when following ends.
    var waitingSong: SongRow? { get }
    var autoplayShown: AutoplayShown { get }
    /// Songs in the queue's Autoplay section, after Up Next.
    var autoplayCount: Int { get }
    /// What Autoplay's songs are based on. That's what the listener started, else the song the context is anchored on.
    var autoplayBasedOn: String? { get }
    func autoplay(at offset: Int) -> QueueEntry?
    /// Removes Autoplay songs, by offset in their section, for this session.
    func removeAutoplay(_ offsets: [Int])
    /// Plays an Autoplay song now. Up Next stays as it was.
    func playAutoplay(_ offset: Int)
    /// A drag within the queue. `rows` are offsets into Up Next followed by Autoplay, and `before` is an Up Next
    /// offset. Moved songs join the section they land in.
    func moveInQueue(_ rows: [Int], before: Int, queued: Bool)
    /// Songs dropped on the queue from another list, at `at` among the songs after the one playing.
    /// `queued` puts them in Next in Queue, otherwise they join the list.
    func queueSongs(_ songs: [SongRow], at: Int, queued: Bool)
    /// The "Next in Queue" songs waiting right after the one playing.
    var queuedCount: Int { get }
    /// Removes those songs and returns them for Undo. The rest of the list plays on.
    @discardableResult func clearQueued() -> [SongRow]
    /// Shuffles those songs once, in place.
    func shuffleQueued()
    /// Removes the rest of the list after Next in Queue. Returns an undo, or nil when there was nothing to clear.
    func clearList() -> (@MainActor () -> Void)?
    /// Removes upcoming songs by offset. List songs are skipped this time through, and the list keeps its order.
    func removeUpcoming(_ offsets: [Int])
    /// What the queue plays from. Nil reads "Up Next".
    var queueSource: PlaySource? { get }
    /// Names the next list played when the page's own name isn't it, such as a radio.
    var nextSource: PlaySource? { get set }
    /// While the queue shows, Autoplay asks the server ahead of time.
    func setQueueVisible(_ visible: Bool)
}

/// The player's repeat mode.
enum PlayerRepeat: String, Codable, Sendable {
    case off, all, one

    var next: PlayerRepeat {
        switch self {
        case .off: .all
        case .all: .one
        case .one: .off
        }
    }

    var title: String {
        switch self {
        case .off: L("Repeat")
        case .all: L("Repeat all")
        case .one: L("Repeat one")
        }
    }

    /// The saved choice. A saved Bool under "repeat" reads as all.
    static func saved(_ defaults: UserDefaults = .standard) -> PlayerRepeat {
        if let raw = defaults.string(forKey: "repeatMode"), let mode = PlayerRepeat(rawValue: raw) { return mode }
        return defaults.bool(forKey: "repeat") ? .all : .off
    }
}

extension LibraryViewModel {
    var syncProgress: SyncProgressRow? { nil }
    var syncMeterOnPage: Bool { false }
    func syncMeterVisible(_ on: Bool) {}
    func savePlaylistOrder(id: String, expected: [String], order: [String]) async -> PlaylistEdit { .saved }
    func storedPlaylistSongs(id: String) async -> [SongRow]? { nil }
    func deletePlaylist(id: String) async -> String? { L("Deleting a playlist needs the server.") }
    func playlistSongsNow(id: String) -> [SongRow] { songs(forPlaylist: id).entireList() }
    func playlistSongsToPlay(id: String) async -> [SongRow] { playlistSongsNow(id: id) }
    func playlistTargets() -> [PlaylistRef] { [] }
    func addToPlaylist(id: String, songs: [SongRow]) async -> PlaylistEdit { .failed(L("Playlists can't be changed without the server.")) }
    func alreadyInPlaylist(id: String, songs: [SongRow]) -> Set<String> { [] }
    func removeFromPlaylist(id: String, expected: [String], places: [Int]) async -> PlaylistEdit { .failed(L("Playlists can't be changed without the server.")) }
    var state: LibraryState { .ready }
    var isOffline: Bool { false }
    var quality: QualitySettings? { nil }
    func retryConnection() async -> Bool { true }
    func refreshNow() {}
    func isRefreshing(library: Bool) -> Bool { false }
    var offlineSwitch: Bool { false }
    func setOfflineSwitch(_ on: Bool) {}
    func downloadProgress(song: SongRow) -> Double? { nil }
    func isDownloading(song: SongRow) -> Bool { downloadProgress(song: song) != nil }
    func radioQueue(startingFrom song: SongRow) async -> [SongRow]? { radioQueue(for: song) }
    func radioSongs(after song: SongRow) async -> [SongRow] { Array((radioQueue(for: song) ?? []).dropFirst()) }
    func artistRadio(for artist: ArtistRow) async -> [SongRow] { similarSongs(for: artist) }
    func knownNoRadio(_ song: SongRow) -> Bool { false }
    func recommender(forPlaylist id: String) -> (() async -> [SongRow])? { nil }
    func setReporting(_ on: Bool) {}
    func setBackgroundSync(_ on: Bool) {}
    var downloadedCount: Int { 0 }
    func downloadEntries() async -> [DownloadEntry] { [] }
    var synced: SyncedDownloads? { nil }
}

extension PlayerViewModel {
    func playAsRadio(_ song: SongRow, radio: @escaping @MainActor () async -> [SongRow]) {
        Task { @MainActor [weak self] in
            let songs = await radio()
            self?.play([song] + songs, at: 0, shuffle: false)
        }
    }
    var autoplayEnabled: Bool { get { false } set {} }
    var autoplayMode: AutoplayModeRow { get { .navia } set {} }
    var shuffleStyle: ShuffleStyle { get { .fewerRepeats } set {} }
    var matchSampleRate: Bool { get { true } set {} }
    var shareListening: Bool { get { false } set {} }
    var listen: ListenAlongModel? { nil }
    var followingFriend: String? { nil }
    var followingWaiting: Bool { false }
    var followingHeld: Bool { false }
    var followingSending: Bool { false }
    var sessionOn: Bool { false }
    var sessionTheirs: [String] { [] }
    var followingUser: String? { nil }
    func stopFollowing() {}
    var waitingSong: SongRow? { nil }
    var autoplayShown: AutoplayShown { .hidden }
    var autoplayCount: Int { 0 }
    var autoplayBasedOn: String? { nil }
    func autoplay(at offset: Int) -> QueueEntry? { nil }
    func removeAutoplay(_ offsets: [Int]) {}
    func playAutoplay(_ offset: Int) {}
    func moveInQueue(_ rows: [Int], before: Int, queued: Bool) {
        let up = rows.filter { $0 < upcomingCount }
        if !up.isEmpty { moveUpcoming(up, before: before) }
    }
    var queuedCount: Int { 0 }
    func queueSongs(_ songs: [SongRow], at: Int, queued: Bool) { songs.forEach(addToQueue) }
    func clearQueued() -> [SongRow] { [] }
    func shuffleQueued() {}
    func clearList() -> (@MainActor () -> Void)? { nil }
    func removeUpcoming(_ offsets: [Int]) {}
    var queueSource: PlaySource? { nil }
    var nextSource: PlaySource? { get { nil } set {} }
    func setQueueVisible(_ visible: Bool) {}
    var isWaitingForServer: Bool { false }
    var queueOfferDevice: String? { nil }
    func answerQueueOffer(accept: Bool) {}
    var quality: AudioQuality? { nil }
    func retryServer() {}
    func setCrossfade(ms: Int, smart: Bool, mix: Bool) {}
    func setOutputDevice(_ id: String?) {}
    func applyEqualizer(_ plan: EqPlan?) {}
    func setDiscordStatus(_ on: Bool) {}
    func setDiscordPublicAddress(_ text: String) {}
    func setDiscordCoversFrom(_ value: String) {}
    func setViewsLive(_ live: Bool) {}
}

/// `0:00` style durations.
func clock(_ seconds: Double) -> String {
    let s = max(0, Int(seconds))
    return "\(s / 60):\(String(format: "%02d", s % 60))"
}


func songCount(_ n: Int) -> String { Ln("{count} Song", "{count} Songs", n) }
func albumCount(_ n: Int) -> String { Ln("{count} Album", "{count} Albums", n) }

/// A genre as the Genres page shows it, with its album count and the covers picked for it.
nonisolated struct GenreEntry: Hashable, Sendable {
    let name: String
    let albums: Int
    let covers: [URL]
}

/// "Your Top Genres", every genre as a row, and Search's Browse by Genre tiles.
nonisolated struct GenrePage: Sendable {
    var top: [GenreEntry] = []
    var all: [GenreEntry] = []
    var browse: [GenreEntry] = []
}

func longDuration(_ seconds: Int) -> String {
    let m = max(seconds > 0 ? 1 : 0, Int((Double(seconds) / 60).rounded()))
    if m >= 60 { return L("{hours} hr {minutes} min", ["hours": fmtInt(m / 60), "minutes": fmtInt(m % 60)]) }
    return Ln("{count} minute", "{count} minutes", m)
}

/// "12 Songs, 45 minutes", or "No songs" for an empty list.
func songsAndLength(_ n: Int, seconds: Int) -> String {
    n == 0 ? L("No songs") : L("{songs}, {length}", ["songs": songCount(n), "length": longDuration(seconds)])
}

extension LibraryViewModel {
    var shareLinks: ShareLinks? { nil }
}
