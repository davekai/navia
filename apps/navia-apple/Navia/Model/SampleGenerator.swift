import Foundation

/// A made-up library for sample mode, the same as GPUI's `FakeLibrary` in `sample.rs`. Ids use `_` instead of `-`,
/// because a `-n` suffix marks a `-sampleScale` copy here.
extension SampleLibrary.File {
    /// Enough songs for full shelves and long tables while sample mode stays quick to build.
    static let generatedSongCount = 2_000
    /// The tint for an album without a cover.
    static let generatedTint = "#2a2b33"
    static let generatedPlaylistCount = 2
    static let generatedPlaylistSize = 50
    /// matches GPUI's `top_songs` cap.
    static let generatedTopSongs = 20

    static func generated(count: Int = generatedSongCount) -> Self {
        let words = ["Northern", "Paper", "Slow", "Glass", "River", "Static", "Morning", "Velvet", "Hollow", "Golden", "Distant", "Quiet", "Electric", "Lonely", "Silver", "Winter"]
        let nouns = ["Lights", "Birds", "Signal", "Harbor", "Ghosts", "Season", "Echoes", "Rooms", "Fever", "Tide", "Promise", "Roads"]
        let genreNames = ["Electronic", "Pop", "Indie Rock", "Hip Hop", "Jazz", "R&B", "Ambient"]
        var rng: UInt32 = 0x1357_9bdf
        func next() -> Int {
            rng ^= rng << 13
            rng ^= rng >> 17
            rng ^= rng << 5
            return Int(rng)
        }

        var songs: [Song] = []
        songs.reserveCapacity(count)
        for i in 0..<count {
            let a = next(), b = next(), c = next()
            songs.append(Song(id: "fake_\(i)", title: "\(words[c % words.count]) \(nouns[a % nouns.count])",
                              artist: "\(words[a % words.count]) \(nouns[b % nouns.count])",
                              album: "\(words[b % words.count]) \(nouns[c % nouns.count])", albumID: "fake_album_\(c % 900)",
                              duration: 120 + b % 300, genre: genreNames[(a ^ b) % genreNames.count], year: 1980 + a % 45,
                              plays: count - i, isFavorite: a % 7 == 0, isDownloaded: b % 5 == 0, number: nil,
                              hasLyrics: c % 2 == 0, hiRes: false))
        }

        // An album takes its name, artist, year and genre from its first song.
        var albumOrder: [String] = []
        var tracks: [String: [Song]] = [:]
        for s in songs {
            if tracks[s.albumID] == nil { albumOrder.append(s.albumID) }
            tracks[s.albumID, default: []].append(s)
        }
        var albums: [String: Album] = [:]
        for id in albumOrder {
            let list = tracks[id] ?? [], first = list[0]
            albums[id] = Album(id: id, name: first.album, artist: first.artist, year: first.year, songCount: list.count,
                               duration: list.reduce(0) { $0 + $1.duration }, genre: first.genre, tint: generatedTint, art: "", blurb: "",
                               added: "", plays: list.reduce(0) { $0 + $1.plays }, last: nil, isFavorite: false, tracks: list)
        }
        // Most played first, then by name.
        let byPlays = albums.values.sorted { ($0.plays, $1.name) > ($1.plays, $0.name) }

        var artistSongs: [String: [Song]] = [:], artistOrder: [String] = []
        for s in songs {
            if artistSongs[s.artist] == nil { artistOrder.append(s.artist) }
            artistSongs[s.artist, default: []].append(s)
        }
        let artists = artistOrder.map { name -> Artist in
            let own = artistSongs[name] ?? []
            var seen = Set<String>(), top: [Song] = []
            for s in own where top.count < generatedTopSongs && seen.insert(s.title.lowercased()).inserted { top.append(s) }
            let albumIDs = byPlays.filter { $0.artist == name }.sorted { ($0.year ?? 0) > ($1.year ?? 0) }.map(\.id)
            return Artist(id: name, name: name, photo: "", bio: "", plays: own.reduce(0) { $0 + $1.plays }, albums: albumIDs, similar: [],
                          isFavorite: false, top: top)
        }.sorted { ($0.plays, $1.name) > ($1.plays, $0.name) }

        // Plain playlists of every n-th song.
        let playlists = (0..<generatedPlaylistCount).map { p -> Playlist in
            let list = Array(songs.enumerated().filter { $0.offset % generatedPlaylistCount == p }.map(\.element).prefix(generatedPlaylistSize))
            return Playlist(id: "p\(p)", name: "Sample Mix \(p + 1)", summary: "", owner: "", songCount: list.count,
                            duration: list.reduce(0) { $0 + $1.duration }, isSmart: false, isPublic: true, rules: nil, tracks: list)
        }

        var genreCounts: [String: Int] = [:]
        for s in songs { genreCounts[s.genre, default: 0] += 1 }
        let genres = genreCounts.sorted { ($0.value, $1.key) > ($1.value, $0.key) }.map(\.key)

        return Self(songs: songs, nowPlaying: songs[0], lyrics: [], albums: albums, artists: artists, playlists: playlists,
                    recent: [], top: byPlays.map(\.id), genres: genres, radio: [], user: "")
    }
}
