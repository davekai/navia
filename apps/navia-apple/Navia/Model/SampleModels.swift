import Foundation
import Observation
import NaviaCoreFFI

/// Sample data from `SampleData/library.json`, bundled at build time when the folder exists.
/// Without it (a public checkout), the made-up library from `SampleGenerator.swift`.
/// Nothing here talks to a server. Launch with `-sampleScale 40` to repeat the rows for a scale test.
@Observable final class SampleLibrary: LibraryViewModel {
    var serverLabel: String { "Sample data" }
    var reportsPlays: Bool { false }
    var downloadedCount: Int { collection.downloadedSongs.count }
    /// Sizes are estimates from the length, because the sample has no files.
    func downloadEntries() async -> [DownloadEntry] {
        allRows.filter { collection.downloadedSongs.contains($0.id) }.sorted { $0.title.localizedStandardCompare($1.title) == .orderedAscending }
            .map { DownloadEntry(song: collection.apply($0), bytes: Int64($0.duration) * 40_000) }
    }

    struct File: Decodable {
        struct Album: Decodable {
            let id, name, artist: String
            let year: Int?
            let songCount, duration: Int
            let genre, tint, art, blurb, added: String
            let plays: Int
            let last: String?
            let isFavorite: Bool
            let tracks: [Song]
        }
        struct Artist: Decodable {
            let id, name, photo, bio: String
            let plays: Int
            let albums, similar: [String]
            let isFavorite: Bool
            let top: [Song]
        }
        struct Rules: Decodable { let match: String; let rules, order: [String] }
        struct Playlist: Decodable {
            let id, name, summary, owner: String
            let songCount, duration: Int
            let isSmart, isPublic: Bool
            let rules: Rules?
            let tracks: [Song]
        }
        struct Song: Decodable {
            let id, title, artist, album, albumID: String
            let duration: Int
            let genre: String
            let year: Int?
            let plays: Int
            let isFavorite, isDownloaded: Bool
            let number: Int?
            let hasLyrics, hiRes: Bool
        }
        /// `w`: the server's word timing, per word [start ms, end ms or null, UTF-8 byte start, byte end (exclusive)] of `t`.
        struct Line: Decodable { let s: Int?; let t: String; let w: [[Int?]]? }
        struct Station: Decodable { let name, url: String }
        let songs: [Song]
        let nowPlaying: Song
        let lyrics: [Line]
        let albums: [String: Album]
        let artists: [Artist]
        let playlists: [Playlist]
        let recent, top, genres: [String]
        let radio: [Station]
        let user: String
    }

    private struct Artist {
        let row: ArtistRow
        let plays: Int
        let albums: [String]
        let similar: [String]
        let top: [SongRow]
    }

    private struct Album {
        let row: AlbumRow
        let plays: Int
        let added: String
        let last: Date?
        let tracks: [SongRow]
    }

    private struct Playlist {
        let row: PlaylistRow
        let tracks: [SongRow]
    }

    @ObservationIgnored let collection: LocalCollection
    @ObservationIgnored let allRows: [SongRow]
    @ObservationIgnored let allSongs: any SongsViewModel
    @ObservationIgnored let nowPlaying: SongRow?
    @ObservationIgnored let nowLyrics: [LyricLine]
    @ObservationIgnored let artistPhotos: [String: URL]
    @ObservationIgnored let artistIDs: [String: String]
    @ObservationIgnored let playlists: [PlaylistRef]
    @ObservationIgnored let genres: [String]
    @ObservationIgnored let stations: [RadioStation]
    @ObservationIgnored private let albumsByID: [String: Album]
    @ObservationIgnored private let albumOrder: [String]
    @ObservationIgnored private let artistsByID: [String: Artist]
    @ObservationIgnored private let artistList: [Artist]
    @ObservationIgnored private let playlistsByID: [String: Playlist]
    @ObservationIgnored private let playlistOrder: [String]
    @ObservationIgnored private let recentIDs: [String]
    @ObservationIgnored private let topIDs: [String]
    @ObservationIgnored private let scale: Int
    @ObservationIgnored private var similarCache: [String: [SongRow]] = [:]
    @ObservationIgnored private var songsByAlbum: [String: [SongRow]]

    var collectionRevision: Int { collection.revision }

    /// rows repeated by the scale test have ids ending in `-n`.
    private func base(_ id: String) -> String { scale > 1 ? String(id.prefix { $0 != "-" }) : id }

    init() {
        let scale = max(1, UserDefaults.standard.integer(forKey: "sampleScale"))
        self.scale = scale
        let root = Bundle.main.url(forResource: "SampleData", withExtension: nil)
        let file = root.flatMap { try? Data(contentsOf: $0.appending(path: "library.json")) }
            .flatMap { try? JSONDecoder().decode(File.self, from: $0) } ?? File.generated()
        func art(_ name: String) -> URL? { name.isEmpty ? nil : root?.appending(path: "art/\(name)") }
        func photo(_ name: String) -> URL? { name.isEmpty ? nil : root?.appending(path: "artist/\(name)") }

        var photos: [String: URL] = [:], ids: [String: String] = [:]
        for a in file.artists {
            if let url = photo(a.photo) { photos[a.name] = url }
            ids[a.name] = a.id
        }
        artistPhotos = photos
        artistIDs = ids

        func row(_ s: File.Song, suffix: String = "") -> SongRow {
            let album = file.albums[s.albumID]
            return SongRow(id: s.id + suffix, title: s.title, artist: s.artist, album: s.album, albumID: s.albumID + suffix,
                           duration: s.duration, genre: s.genre, year: s.year, plays: s.plays,
                           isFavorite: s.isFavorite, isDownloaded: s.isDownloaded,
                           artworkURL: album.flatMap { art($0.art) }, tintHex: album?.tint,
                           trackNumber: s.number, hasLyrics: s.hasLyrics, isHiRes: s.hiRes)
        }

        var rows: [SongRow] = []
        rows.reserveCapacity(file.songs.count * scale)
        for n in 0..<scale { rows += file.songs.map { row($0, suffix: n == 0 ? "" : "-\(n)") } }
        allRows = rows
        nowPlaying = row(file.nowPlaying)
        nowLyrics = file.lyrics.map { line in
            let words = (line.w ?? []).compactMap { w -> LyricWord? in
                guard w.count == 4, let start = w[0], let from = w[2], let to = w[3] else { return nil }
                return LyricWord(start: UInt32(from), end: UInt32(to), startMs: UInt32(start), endMs: w[1].map { UInt32($0) })
            }
            return LyricLine(start: line.s, text: line.t, words: words)
        }

        var byAlbum: [String: [SongRow]] = [:]
        for s in rows where !s.id.contains("-") { byAlbum[s.albumID, default: []].append(s) }
        for (key, value) in byAlbum { byAlbum[key] = value.sorted { ($0.trackNumber ?? 0) < ($1.trackNumber ?? 0) } }
        songsByAlbum = byAlbum

        var favoriteSongRows = rows.filter(\.isFavorite), downloadedRows = rows.filter(\.isDownloaded)
        var albums: [String: Album] = [:]
        var favAlbums = Set<String>()
        let isoDate = ISO8601DateFormatter()
        for a in file.albums.values {
            let tracks = a.tracks.map { row($0) }
            favoriteSongRows += tracks.filter(\.isFavorite)
            downloadedRows += tracks.filter(\.isDownloaded)
            if a.isFavorite { favAlbums.insert(a.id) }
            albums[a.id] = Album(row: AlbumRow(id: a.id, name: a.name, artist: a.artist, artistID: ids[a.artist], year: a.year,
                                               songCount: a.songCount, duration: a.duration, genre: a.genre,
                                               artworkURL: art(a.art), tintHex: a.tint, blurb: a.blurb, isFavorite: a.isFavorite),
                                 plays: a.plays, added: a.added, last: a.last.flatMap { isoDate.date(from: $0) ?? Self.lastDate($0) }, tracks: tracks)
        }
        albumsByID = albums
        albumOrder = file.albums.values.sorted { ($0.plays, $1.name) > ($1.plays, $0.name) }.map(\.id)

        var favArtists = Set<String>()
        var artists: [Artist] = []
        for a in file.artists {
            if a.isFavorite { favArtists.insert(a.id) }
            let tint = a.albums.first.flatMap { file.albums[$0]?.tint }
            let tops = a.top.map { row($0) }
            favoriteSongRows += tops.filter(\.isFavorite)
            downloadedRows += tops.filter(\.isDownloaded)
            artists.append(Artist(row: ArtistRow(id: a.id, name: a.name, photoURL: photo(a.photo), tintHex: tint, bio: a.bio, isFavorite: a.isFavorite),
                                  plays: a.plays, albums: a.albums, similar: a.similar, top: tops))
        }
        artistList = artists
        artistsByID = Dictionary(artists.map { ($0.row.id, $0) }, uniquingKeysWith: { a, _ in a })

        var lists: [String: Playlist] = [:]
        for p in file.playlists {
            let tracks = p.tracks.map { row($0) }
            favoriteSongRows += tracks.filter(\.isFavorite)
            downloadedRows += tracks.filter(\.isDownloaded)
            var seen = Set<URL>(), collage: [URL] = []
            for t in tracks { if let url = t.artworkURL, seen.insert(url).inserted { collage.append(url) }; if collage.count == 4 { break } }
            lists[p.id] = Playlist(row: PlaylistRow(id: p.id, name: p.name, songCount: p.songCount, duration: p.duration, summary: p.summary, isSmart: p.isSmart, isPublic: p.isPublic,
                                                    rules: p.rules.map { RuleSummary(match: $0.match, rules: $0.rules, order: $0.order) },
                                                    isOwned: p.owner.lowercased() == file.user.lowercased(), ownerName: p.owner,
                                                    isMine: PlaylistSection.owns(p.owner, me: file.user), collage: collage,
                                                    canReorder: !p.isSmart && p.owner.lowercased() == file.user.lowercased()),
                                   tracks: tracks)
        }
        playlistsByID = lists
        playlistOrder = file.playlists.map(\.id)
        playlists = playlistOrder.compactMap { lists[$0] }.map { p in PlaylistRef(id: p.row.id, name: p.row.name, isMine: p.row.isMine, isPublic: p.row.isPublic, isSmart: p.row.isSmart, collage: p.row.collage) }

        recentIDs = file.recent
        topIDs = file.top
        genres = file.genres
        stations = file.radio.map { RadioStation(name: $0.name, url: $0.url) }

        let collection = LocalCollection(favorites: favoriteSongRows, downloaded: downloadedRows, favoriteAlbums: favAlbums, favoriteArtists: favArtists)
        self.collection = collection
        allSongs = LocalSongsModel(rows: rows, collection: collection)
    }

    private static func lastDate(_ text: String) -> Date? {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = TimeZone(secondsFromGMT: 0)
        f.dateFormat = "yyyy-MM-dd HH:mm:ss"
        return f.date(from: String(text.prefix(19)).replacingOccurrences(of: "T", with: " "))
    }

    private func applied(_ album: AlbumRow) -> AlbumRow {
        var out = album
        out.isFavorite = collection.favoriteAlbums.contains(album.id)
        return out
    }

    private func applied(_ artist: ArtistRow) -> ArtistRow {
        var out = artist
        out.isFavorite = collection.favoriteArtists.contains(artist.id)
        return out
    }

    /// Each sample album has one genre. A simple stand-in for the core's genre page.
    var genrePage: GenrePage {
        let byGenre = Dictionary(grouping: albumsByID.values.filter { !$0.row.genre.isEmpty }, by: \.row.genre)
        let ranked = byGenre.map { (name: $0.key, albums: $0.value.sorted { ($0.plays, $0.added) > ($1.plays, $1.added) }, plays: $0.value.reduce(0) { $0 + $1.plays }) }
            .sorted { ($0.plays, $0.albums.count, $1.name) > ($1.plays, $1.albums.count, $0.name) }
        var used = Set<URL>()
        let top = ranked.filter { $0.plays > 0 }.prefix(12).map { g in
            let covers = g.albums.compactMap(\.row.artworkURL).filter { !used.contains($0) }.prefix(3)
            used.formUnion(covers)
            return GenreEntry(name: g.name, albums: g.albums.count, covers: Array(covers))
        }
        let topNames = Set(top.map(\.name))
        let browse = ranked.filter { !topNames.contains($0.name) }.sorted { ($0.albums.count, $1.name) > ($1.albums.count, $0.name) }.map { g in
            let art = g.albums.compactMap(\.row.artworkURL)
            let covers = (art.filter { !used.contains($0) } + art.filter { used.contains($0) }).prefix(3)
            used.formUnion(covers)
            return GenreEntry(name: g.name, albums: g.albums.count, covers: Array(covers))
        }
        return GenrePage(top: top, all: ranked.map { GenreEntry(name: $0.name, albums: $0.albums.count, covers: Array($0.albums.compactMap(\.row.artworkURL).prefix(1))) },
                         browse: browse)
    }

    func album(id: String) -> AlbumRow? { albumsByID[base(id)].map { applied($0.row) } }
    func artist(id: String) -> ArtistRow? { artistsByID[base(id)].map { applied($0.row) } }
    func artist(named name: String) -> ArtistRow? { artistIDs[name].flatMap { artist(id: $0) } }
    /// `-naviaSmartFixture owned|other` adds a smart playlist with rules from `-naviaSmartRules`, so the editor can be
    /// checked without the server. Sample mode only.
    static let smartFixtureRules = UserDefaults.standard.string(forKey: "naviaSmartRulesFile").flatMap { try? String(contentsOfFile: $0, encoding: .utf8) } ?? #"{"all":[{"is":{"genre":"Rock"}},{"inTheRange":{"year":[1990,1999]}}],"sort":"playcount","order":"desc","limit":50}"#

    private var smartFixture: PlaylistRow? {
        guard let kind = UserDefaults.standard.string(forKey: "naviaSmartFixture") else { return nil }
        return PlaylistRow(id: "fixture-smart", name: "Weekend Mix", songCount: 40, duration: 7200, summary: "", isSmart: true, isPublic: true,
                           rules: smartRuleSummary(Self.smartFixtureRules, playlists: []), isOwned: kind == "owned", ownerName: kind == "owned" ? "" : "someone", isMine: kind == "owned")
    }

    func smartRulesJSON(playlistID: String) -> String? { playlistID == "fixture-smart" ? Self.smartFixtureRules : nil }

    func smartPreview(editor: NaviaSmartEditor, maxRows: Int) async -> (preview: SmartPreviewResult, songs: [SongRow]) {
        let songs = allSongs.entireList()
        let preview = editor.previewSample(songs: songs.enumerated().map { index, song in
            SmartSampleSong(rowid: UInt32(index), id: song.id, title: song.title, album: song.album, artist: song.artist, genre: song.genre, year: song.year.map { UInt32(max(0, $0)) },
                            plays: UInt32(max(0, song.plays)), loved: song.isFavorite, durationSeconds: UInt32(max(0, song.duration)))
        }, max: UInt32(maxRows))
        return (preview, preview.rowids.compactMap { songs.indices.contains(Int($0)) ? songs[Int($0)] : nil })
    }

    func smartRulesProblem(name: String, criteria: String) -> String? { L("Saving needs the server.") }
    func saveSmartPlaylist(id: String?, name: String, criteria: String) async -> Result<String, SmartSaveFailure> { .failure(SmartSaveFailure(message: L("Saving needs the server."))) }

    func playlist(id: String) -> PlaylistRow? {
        if id == "fixture-smart", let fixture = smartFixture { return fixture }
        guard var row = playlistsByID[base(id)]?.row else { return nil }
        return row
    }

    private func albumRows(_ ids: [String]) -> [AlbumRow] { ids.compactMap { album(id: $0) } }
    private func sampleAlbums(scope: AlbumScope) -> [AlbumRow] {
        switch scope {
        case .all:
            let ordered = albumOrder.compactMap { albumsByID[$0]?.row }
            return scale == 1 ? ordered : (0..<scale).flatMap { n in ordered.map { Self.rename($0, suffix: n) } }
        // The sample has no release types, so an artist's own albums are all Albums.
        case .artist(let id, let section): return section == .albums ? albumRows(artistsByID[base(id)]?.albums ?? []) : []
        }
    }

    private static func rename(_ a: AlbumRow, suffix n: Int) -> AlbumRow {
        guard n > 0 else { return a }
        return AlbumRow(id: "\(a.id)-\(n)", name: a.name, artist: a.artist, artistID: a.artistID, year: a.year, songCount: a.songCount, duration: a.duration, genre: a.genre, artworkURL: a.artworkURL, tintHex: a.tintHex, blurb: a.blurb, isFavorite: a.isFavorite)
    }

    func albums(scope: AlbumScope) -> any AlbumsViewModel {
        SampleAlbumsModel(library: self, scope: scope, rows: sampleAlbums(scope: scope), collection: collection,
                          addedOrder: Dictionary(albumsByID.values.map { ($0.row.id, $0.added) }, uniquingKeysWith: { a, _ in a }),
                          plays: Dictionary(albumsByID.values.map { ($0.row.id, $0.plays) }, uniquingKeysWith: { a, _ in a }))
    }

    func artists() -> any ArtistsViewModel {
        let rows = artistList.map(\.row)
        let scaled = scale == 1 ? rows : (0..<scale).flatMap { n in rows.map { n == 0 ? $0 : ArtistRow(id: "\($0.id)-\(n)", name: $0.name, photoURL: $0.photoURL, tintHex: $0.tintHex, bio: $0.bio, isFavorite: $0.isFavorite) } }
        return SampleArtistsModel(rows: scaled)
    }

    func playlistsList() -> any PlaylistsViewModel {
        let rows = playlistOrder.compactMap { playlistsByID[$0]?.row }
        let scaled = scale == 1 ? rows : (0..<scale).flatMap { n in rows.map { p in
            n == 0 ? p : PlaylistRow(id: "\(p.id)-\(n)", name: p.name, songCount: p.songCount, duration: p.duration, summary: p.summary, isSmart: p.isSmart, isPublic: p.isPublic, rules: p.rules, isOwned: p.isOwned, ownerName: p.ownerName, isMine: p.isMine, coverURL: p.coverURL, collage: p.collage)
        } }
        return SamplePlaylistsModel(rows: scaled, collection: collection)
    }

    func songs(forAlbum id: String) -> any SongsViewModel {
        LocalSongsModel(rows: tracks(forAlbum: id), collection: collection, defaultSort: .order)
    }

    func tracks(forAlbum id: String) -> [SongRow] {
        let album = albumsByID[base(id)]
        return album?.tracks.isEmpty == false ? album!.tracks : songsByAlbum[base(id)] ?? []
    }

    func songs(forPlaylist id: String) -> any SongsViewModel {
        LocalSongsModel(rows: playlistsByID[base(id)]?.tracks ?? [], collection: collection, defaultSort: .order)
    }

    func songs(forGenre genre: String) -> any SongsViewModel {
        LocalSongsModel(rows: allRows.filter { $0.genre == genre }, collection: collection)
    }

    func likedSongs() -> any SongsViewModel {
        let model = LocalSongsModel(rows: [], collection: collection, defaultSort: .liked)
        let rows = Dictionary(allRows.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        model.followFavorites { ids in ids.compactMap { rows[$0] } }
        return model
    }

    private func dedupedTop(_ a: Artist) -> [SongRow] {
        var seen = Set<String>()
        return a.top.map { collection.apply($0) }.filter { seen.insert($0.title.lowercased().trimmingCharacters(in: .whitespaces)).inserted }
    }

    // Essentials tiles need at least this many distinct top songs.
    private let essentialsMinimum = 8

    func essentialsArtists(limit: Int) -> [ArtistRow] {
        Array(artistList.filter { $0.plays > 0 && dedupedTop($0).count >= essentialsMinimum }.prefix(limit).map { applied($0.row) })
    }

    func essentialsSongs(for artist: ArtistRow) -> [SongRow] {
        artistsByID[base(artist.id)].map { dedupedTop($0) } ?? []
    }

    func worldOf() -> (artist: ArtistRow, albums: [AlbumRow])? {
        guard let a = artistList.first(where: { $0.plays > 0 && $0.albums.count >= 4 }) else { return nil }
        return (applied(a.row), albumRows(a.albums))
    }

    func recentAlbums(limit: Int) -> [AlbumRow] { albumRows(Array(recentIDs.prefix(limit))) }
    var accountKey: String { "sample" }

    func topAlbums(limit: Int) -> [AlbumRow] { albumRows(Array(topIDs.filter { (albumsByID[$0]?.plays ?? 0) > 0 }.prefix(limit))) }

    /// For You without a core, with only Not Played Recently from the sample's album dates.
    func forYou() -> ForYouRows? {
        let cutoff = Date().addingTimeInterval(-90 * 86_400)
        let stale = albumsByID.values.filter { a in a.plays >= 3 && (a.last.map { $0 < cutoff } ?? false) }
        return ForYouRows(notPlayedRecently: stale.sorted { ($0.last ?? .distantPast) < ($1.last ?? .distantPast) }.prefix(20).map { applied($0.row) })
    }

    func mix(id: String) -> MixInfo? { nil }
    func songs(forMix mix: MixInfo) -> any SongsViewModel { LocalSongsModel(rows: mix.songs, collection: collection, defaultSort: .order) }
    func songs(forArtistTop rows: [SongRow]) -> any SongsViewModel { LocalSongsModel(rows: rows, collection: collection, defaultSort: .order, sortable: false) }
    func songs(forUpcoming album: UpcomingAlbum) -> any SongsViewModel { LocalSongsModel(rows: album.tracks, collection: collection, defaultSort: .order) }

    func latestSongs(limit: Int) -> [SongRow] {
        let firstTracks: [SongRow] = recentIDs.compactMap { id in
            let tracks = albumsByID[id]?.tracks.isEmpty == false ? albumsByID[id]!.tracks : songsByAlbum[id] ?? []
            return tracks.first
        }
        return firstTracks.prefix(limit).map { collection.apply($0) }
    }

    func topSongs(limit: Int) -> [SongRow] { allRows.prefix(limit).map { collection.apply($0) } }
    func topArtists(limit: Int) -> [ArtistRow] { artistList.filter { $0.plays > 0 }.prefix(limit).map { applied($0.row) } }

    private func similar(_ a: Artist) -> [SongRow] {
        if let hit = similarCache[a.row.id] { return hit }
        let names = Set(a.similar)
        var found = names.isEmpty ? [] : allRows.filter { song in Self.credited(song.artist).contains(where: names.contains) }
        found = found.count >= 5 ? Array(found.sorted { Self.hash(a.row.id + $0.id) < Self.hash(a.row.id + $1.id) }.prefix(40)) : []
        similarCache[a.row.id] = found
        return found
    }

    /// Artist Radio from the sample's similar artists by name, following the core's rules for stations.
    func artistStations() -> [ArtistStation] {
        let byName = Dictionary(artistList.map { ($0.row.name, $0) }, uniquingKeysWith: { a, _ in a })
        return artistList.filter { $0.plays > 0 }.prefix(20).compactMap { seed -> ArtistStation? in
            var seen = Set([seed.row.id])
            let neighbours = seed.similar.compactMap { byName[$0] }.filter { $0.row.photoURL != nil && seen.insert($0.row.id).inserted }
            return neighbours.count >= 2 ? ArtistStation(seed: applied(seed.row), neighbours: neighbours.map { applied($0.row) }) : nil
        }.prefix(10).map { $0 }
    }

    func radioArtists(limit: Int) -> [ArtistRow] {
        Array(artistList.filter { !similar($0).isEmpty }.prefix(limit).map { applied($0.row) })
    }

    func topSongs(for artist: ArtistRow) -> [SongRow] { essentialsSongs(for: artist) }
    func playableSongs(for artist: ArtistRow) -> [SongRow] {
        let top = topSongs(for: artist)
        if !top.isEmpty { return top }
        return (artistsByID[base(artist.id)]?.albums ?? []).flatMap { id in
            (albumsByID[id]?.tracks.isEmpty == false ? albumsByID[id]!.tracks : songsByAlbum[id] ?? []).map { collection.apply($0) }
        }
    }
    func albums(for artist: ArtistRow) -> [AlbumRow] { albumRows(artistsByID[base(artist.id)]?.albums ?? []) }
    func discography(for artist: ArtistRow) -> [DiscographyPart]? {
        let own = albums(for: artist)
        return own.isEmpty ? [] : [DiscographyPart(section: .albums, albums: own)]
    }
    func similarSongs(for artist: ArtistRow) -> [SongRow] { artistsByID[base(artist.id)].map { similar($0).map { collection.apply($0) } } ?? [] }

    func songs(by artist: ArtistRow, matching query: String) -> [SongRow] {
        let needle = query.lowercased()
        let collection = self.collection
        return allRows.lazy.filter { !$0.id.contains("-") && Self.credited($0.artist).contains(artist.name) && "\($0.title) \($0.album)".lowercased().contains(needle) }
            .prefix(12).map { collection.apply($0) }
    }

    func radioQueue(for song: SongRow) -> [SongRow]? {
        guard let a = artistIDs[song.artist].flatMap({ artistsByID[$0] }) else { return nil }
        let rest = similar(a).filter { $0.id != song.id }
        return rest.isEmpty ? nil : [song] + rest.map { collection.apply($0) }
    }

    static func credited(_ artist: String) -> [String] {
        let pattern = #"\s*(?:,|&|•|\bfeat\.?|\bft\.?|/)\s*"#
        let spaced = artist.replacingOccurrences(of: pattern, with: "\u{1}", options: [.regularExpression, .caseInsensitive])
        return spaced.split(separator: "\u{1}").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
    }

    static func hash(_ s: String) -> UInt32 { s.unicodeScalars.reduce(UInt32(0)) { ($0 &* 31) &+ $1.value } }

    @ObservationIgnored private var lastSearch: (key: String, results: SearchResults, revision: Int)?

    func searchResults(_ query: String, sort: SearchSort, favoritesOnly: Bool) async -> SearchResults { search(query, sort: sort, favoritesOnly: favoritesOnly) }

    func search(_ query: String, sort: SearchSort, favoritesOnly: Bool) -> SearchResults {
        let key = "\(query)\u{1}\(sort.rawValue)\u{1}\(favoritesOnly)"
        if let last = lastSearch, last.key == key, last.revision == collection.revision { return last.results }
        let needle = query.lowercased()
        let artists = artistList.filter { $0.row.name.lowercased().contains(needle) }.map { applied($0.row) }.filter { !favoritesOnly || $0.isFavorite }
        var albums = albumsByID.values.map { applied($0.row) }.filter { ($0.name.lowercased().contains(needle) || $0.artist.lowercased().contains(needle)) && (!favoritesOnly || $0.isFavorite) }
        switch sort {
        case .name: albums.sort { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
        case .year: albums.sort { ($0.year ?? 0) > ($1.year ?? 0) }
        case .relevance: albums.sort { $0.name.count < $1.name.count } // A stable stand-in for FTS rank.
        }
        let collection = self.collection
        let songs = allRows.lazy.filter { !$0.id.contains("-") }.filter { $0.title.lowercased().contains(needle) || $0.artist.lowercased().contains(needle) }
            .map { collection.apply($0) }.filter { !favoritesOnly || $0.isFavorite }.prefix(12)
        let top = artists.first
        let topRows = top.map { self.topSongs(for: $0) } ?? []
        let results = SearchResults(topArtist: top, topArtistSongs: topRows.filter { !favoritesOnly || $0.isFavorite }, artists: artists, albums: Array(albums.prefix(20)), songs: Array(songs))
        lastSearch = (key, results, collection.revision)
        return results
    }

    func toggleFavorite(album id: String) { collection.toggleFavorite(album: base(id)) }
    func toggleFavorite(artist id: String) { collection.toggleFavorite(artist: base(id)) }
    func toggleFavorite(song: SongRow) { collection.toggleFavorite(song: song.id) }
    func isFavorite(song: SongRow) -> Bool { collection.favoriteSongs.contains(song.id) }
    func isDownloaded(song: SongRow) -> Bool { collection.downloadedSongs.contains(song.id) }
    func setDownloaded(songs: [SongRow], _ downloaded: Bool) { collection.setDownloaded(songs.map(\.id), downloaded) }
    func createPlaylist(name: String, songs: [SongRow]) async throws -> String? { throw CoreCreateError.noServer }
    var canChangePlaylistImages: Bool { false }
    func setPlaylistImage(id: String, path: String) async -> String? { "Changing a cover needs the server." }
    func removePlaylistImage(id: String) async -> String? { "Changing a cover needs the server." }
    func updatePlaylistDetails(id: String, name: String?, comment: String?, isPublic: Bool?) async -> String? { L("Editing a playlist needs the server.") }
}

@Observable final class SampleAlbumsModel: AlbumsViewModel {
    @ObservationIgnored private let library: SampleLibrary
    @ObservationIgnored private let scope: AlbumScope
    @ObservationIgnored private var all: [AlbumRow]
    @ObservationIgnored private let added: [String: String]
    @ObservationIgnored private let plays: [String: Int]
    @ObservationIgnored private let collection: LocalCollection
    private var visible: [AlbumRow] = []
    @ObservationIgnored private var listSongs: LocalSongsModel?

    private(set) var count = 0
    private(set) var totalSongs = 0
    private(set) var revision = 0
    var query = "" { didSet { if query != oldValue { rebuild() } } }
    var sort: AlbumSort { didSet { if sort != oldValue { rebuild() } } }
    var genre: String? = nil { didSet { if genre != oldValue { rebuild() } } }

    init(library: SampleLibrary, scope: AlbumScope, rows: [AlbumRow], collection: LocalCollection, addedOrder: [String: String], plays: [String: Int]) {
        self.library = library
        self.scope = scope
        all = rows
        added = addedOrder
        self.plays = plays
        self.collection = collection
        sort = .plays
        rebuild()
    }

    private func rebuild() {
        let needle = query.trimmingCharacters(in: .whitespaces).lowercased()
        var found = all.filter { (genre == nil || $0.genre == genre) && (needle.isEmpty || $0.name.lowercased().contains(needle) || $0.artist.lowercased().contains(needle)) }
        switch sort {
        case .name: found.sort { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
        case .year: found.sort { ($0.year ?? 0) > ($1.year ?? 0) }
        case .recent: found.sort { (added[baseID($0.id)] ?? "") > (added[baseID($1.id)] ?? "") }
        case .plays: found.sort { (plays[baseID($0.id)] ?? 0) > (plays[baseID($1.id)] ?? 0) }
        }
        visible = found
        count = found.count
        totalSongs = found.reduce(0) { $0 + $1.songCount }
        revision &+= 1
        listSongs?.setRows(listedTracks)
    }

    private var listedTracks: [SongRow] { visible.flatMap { library.tracks(forAlbum: $0.id) } }

    func listedAlbumsSongs() -> any SongsViewModel {
        if let listSongs { return listSongs }
        let model = LocalSongsModel(rows: listedTracks, collection: collection, defaultSort: .order)
        listSongs = model
        return model
    }

    private func baseID(_ id: String) -> String { String(id.prefix { $0 != "-" }) }

    func album(at index: Int) -> AlbumRow? {
        guard visible.indices.contains(index) else { return nil }
        return visible[index]
    }
}

@Observable final class SampleArtistsModel: ArtistsViewModel {
    @ObservationIgnored private let all: [ArtistRow]
    private var rows: [ArtistRow]
    var count: Int { rows.count }
    var query = "" {
        didSet {
            let needle = query.trimmingCharacters(in: .whitespaces).lowercased()
            rows = needle.isEmpty ? all : all.filter { $0.name.lowercased().contains(needle) }
        }
    }
    /// Sample mode has no filter index, so nothing sets these.
    var filterSettings: [SongFilterSetting] = []
    init(rows: [ArtistRow]) { all = rows; self.rows = rows }
    func artist(at index: Int) -> ArtistRow? { rows.indices.contains(index) ? rows[index] : nil }
    func counts(of id: String) -> ArtistCountsRow? { nil }
}

@Observable final class SamplePlaylistsModel: PlaylistsViewModel {
    @ObservationIgnored private let all: [PlaylistRow]
    private var visible: [PlaylistRow] = []
    private(set) var count = 0
    var query = "" { didSet { if query != oldValue { rebuild() } } }
    var sort: PlaylistSort = .library { didSet { if sort != oldValue { rebuild() } } }
    var kind: PlaylistKind = .all { didSet { if kind != oldValue { rebuild() } } }
    var scope: PlaylistScope = .all { didSet { if scope != oldValue { rebuild() } } }
    var downloadedIDs: Set<String> = [] { didSet { if downloadedIDs != oldValue && scope == .downloaded { rebuild() } } }
    var hasOthers: Bool { all.contains { $0.section != .mine } }

    init(rows: [PlaylistRow], collection: LocalCollection) {
        all = rows
        rebuild()
        collection.listen { [weak self] in self?.rebuild() }
    }

    private func rebuild() {
        let n = query.trimmingCharacters(in: .whitespaces).lowercased()
        var found = all.filter { scope.includes($0, downloaded: downloadedIDs) && (kind == .all || $0.isSmart == (kind == .smart)) && (n.isEmpty || $0.name.lowercased().contains(n)) }
        switch sort {
        case .library: break
        case .name: found.sort { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
        case .songs: found.sort { $0.songCount > $1.songCount }
        }
        visible = found
        count = visible.count
    }

    func playlist(at index: Int) -> PlaylistRow? { visible.indices.contains(index) ? visible[index] : nil }
}

/// Keeps the state a real player would have, but makes no sound. The position just counts up.
@Observable final class SamplePlayerModel: PlayerViewModel {
    private var entries: [QueueEntry] = []
    private var index = 0
    private(set) var isPlaying = false
    private(set) var position: Double = 0
    private(set) var station: RadioStation?
    var shuffle = false
    var repeatMode = PlayerRepeat.off
    var privateMode = false
    var volume: Double { didSet { UserDefaults.standard.set(volume, forKey: "volume") } }
    var isMuted: Bool { didSet { UserDefaults.standard.set(isMuted, forKey: "muted") } }

    @ObservationIgnored private var ticker: Task<Void, Never>?
    @ObservationIgnored private let artistPhotos: [String: URL]
    @ObservationIgnored private let artistIDs: [String: String]
    @ObservationIgnored private let collection: LocalCollection
    @ObservationIgnored private let nowLyrics: [LyricLine]
    @ObservationIgnored private let nowID: String?
    @ObservationIgnored private var nextUID = 1

    init(library: SampleLibrary) {
        artistPhotos = library.artistPhotos
        artistIDs = library.artistIDs
        collection = library.collection
        nowLyrics = library.nowLyrics
        nowID = library.nowPlaying?.id
        let defaults = UserDefaults.standard
        volume = defaults.object(forKey: "volume") as? Double ?? 80
        isMuted = defaults.bool(forKey: "muted")
        if let now = library.nowPlaying {
            setQueue([now] + library.allRows.filter { $0.id != now.id }.prefix(60))
            position = 40 // Comes back paused partway through, as a restored session would.
        }
    }

    private func setQueue(_ songs: [SongRow]) {
        var uid = nextUID
        entries = songs.map { song in defer { uid += 1 }; return QueueEntry(id: uid, song: song) }
        nextUID = uid
    }

    var current: SongRow? {
        guard station == nil, entries.indices.contains(index) else { return station.map { Self.stationRow($0) } }
        return collection.apply(entries[index].song)
    }

    private static func stationRow(_ s: RadioStation) -> SongRow {
        SongRow(id: "station-\(s.url)", title: s.name, artist: "Internet radio", album: "", albumID: "", duration: 0, genre: "", year: nil, plays: 0,
                isFavorite: false, isDownloaded: false, artworkURL: nil, tintHex: nil)
    }

    var lyrics: [LyricLine] { current?.id == nowID ? nowLyrics : [] }
    var lyricsSynced: Bool { true }
    var lyricsPending: Bool { false }

    var featuredArtists: [FeaturedArtist] {
        guard let current, station == nil else { return [] }
        func entry(_ name: String) -> FeaturedArtist {
            FeaturedArtist(name: name, photoURL: artistPhotos[name], artistID: artistIDs[name])
        }
        if artistIDs[current.artist] != nil { return [entry(current.artist)] }
        var seen = Set<String>()
        return SampleLibrary.credited(current.artist).filter { seen.insert($0).inserted }.map { entry($0) }
    }

    func togglePlayPause() {
        guard current != nil else { return }
        isPlaying.toggle()
        ticker?.cancel()
        guard isPlaying else { return }
        ticker = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(200))
                self?.tick()
            }
        }
    }

    private func startIfStopped() { if !isPlaying { togglePlayPause() } }

    private func tick() {
        guard let current else { return }
        position += 0.2
        if station == nil, position >= Double(current.duration) { if repeatMode == .one { position = 0 } else { next() } }
    }

    func next() { move(by: 1) }
    func previous() { move(by: -1) }

    private func move(by step: Int) {
        guard station == nil, !entries.isEmpty else { return }
        index = (index + step + entries.count) % entries.count
        position = 0
    }

    func seek(to seconds: Double) {
        position = min(Double(current?.duration ?? 0), max(0, seconds))
    }

    func toggleFavorite() {
        guard station == nil, let id = current?.id else { return }
        collection.toggleFavorite(song: id)
    }

    func play(songAt rowIndex: Int, in songs: any SongsViewModel) {
        guard songs.song(at: rowIndex) != nil else { return }
        play(songs.allSongs(), at: rowIndex, shuffle: nil)
    }

    func play(_ songs: [SongRow], at at: Int, shuffle wanted: Bool?) {
        guard !songs.isEmpty else { return }
        station = nil
        if let wanted { shuffle = wanted }
        var queue = songs
        var start = min(max(0, at), queue.count - 1)
        if shuffle, queue.count > 1 {
            let first = queue[start]
            queue.remove(at: start)
            queue.shuffle()
            queue.insert(first, at: 0)
            start = 0
        }
        setQueue(queue)
        index = start
        position = 0
        startIfStopped()
    }

    func playNext(_ song: SongRow) {
        let entry = QueueEntry(id: nextUID, song: song)
        nextUID += 1
        entries.insert(entry, at: min(index + 1, entries.count))
    }

    func addToQueue(_ song: SongRow) {
        entries.append(QueueEntry(id: nextUID, song: song))
        nextUID += 1
    }

    func playStation(_ s: RadioStation) {
        station = s
        position = 0
        startIfStopped()
    }

    func stopStation() {
        station = nil
        position = 0
    }

    var upcomingCount: Int { max(0, entries.count - index - 1) }
    func upcoming(at offset: Int) -> QueueEntry? {
        let at = index + 1 + offset
        return entries.indices.contains(at) ? QueueEntry(id: entries[at].id, song: collection.apply(entries[at].song)) : nil
    }
    func jump(toUpcoming offset: Int) {
        let at = index + 1 + offset
        guard entries.indices.contains(at) else { return }
        index = at
        position = 0
        startIfStopped()
    }

    func moveUpcoming(_ offsets: [Int], before: Int) {
        let head = entries[...index]
        var rest = Array(entries[(index + 1)...])
        let order = movedOrder(len: UInt64(rest.count), positions: offsets.map { UInt64($0) }, before: UInt64(before))
        guard !order.isEmpty else { return }
        rest = order.map { rest[Int($0)] }
        entries = Array(head) + rest
    }
}
