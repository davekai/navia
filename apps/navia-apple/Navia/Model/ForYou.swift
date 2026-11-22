import Foundation
import NaviaCoreFFI

/// One of today's For You mixes, made from the server's similar songs once a day. Nothing is saved on the server
/// until Save as Playlist.
nonisolated struct MixInfo: Identifiable, Sendable {
    /// `artist:<id>` or `genre:<name>`, the mix page's address.
    let id: String
    let isGenre: Bool
    let name: String
    let songs: [SongRow]
    /// Main artists in order of first appearance.
    let artists: [String]

    var title: String { L("{name} Mix", ["name": name]) }
    var artistsLine: String {
        let names = artists.prefix(3).joined(separator: ", ")
        return artists.count > 3 ? L("{names} and more", ["names": names]) : names
    }
    var duration: Int { songs.reduce(0) { $0 + $1.duration } }
    /// The first four different album covers, laid out like a playlist's grid.
    var collage: [URL] {
        var seen = Set<URL>(), out: [URL] = []
        for url in songs.compactMap(\.artworkURL) where seen.insert(url).inserted {
            out.append(url)
            if out.count == 4 { break }
        }
        return out
    }
}

/// For You's rows. Each is empty when the server has no data for it.
nonisolated struct ForYouRows: Sendable {
    var mixes: [MixInfo] = []
    var newToYou: [AlbumRow] = []
    var unplayed: [AlbumRow] = []
    var forgotten: [SongRow] = []
    var onThisDay: [AlbumRow] = []
    var notPlayedRecently: [AlbumRow] = []
    /// False for the placeholder shown while the page loads.
    var loaded = true

    var isEmpty: Bool { mixes.isEmpty && newToYou.isEmpty && unplayed.isEmpty && forgotten.isEmpty && onThisDay.isEmpty && notPlayedRecently.isEmpty }
}

nonisolated extension MixInfo {
    init(core m: NaviaCoreFFI.MixRow) {
        self.init(id: m.id, isGenre: m.isGenre, name: m.name, songs: m.songs.map { SongRow(core: $0) }, artists: m.artists)
    }
}

nonisolated extension ForYouRows {
    init(core r: NaviaCoreFFI.ForYouRow) {
        self.init(mixes: r.mixes.map { MixInfo(core: $0) }, newToYou: r.newToYou.map { AlbumRow(core: $0) }, unplayed: r.unplayed.map { AlbumRow(core: $0) },
                  forgotten: r.forgotten.map { SongRow(core: $0) }, onThisDay: r.onThisDay.map { AlbumRow(core: $0) }, notPlayedRecently: r.notPlayedRecently.map { AlbumRow(core: $0) })
        // an empty page while the mixes are being made isn't final.
        loaded = !(r.building && isEmpty)
    }
}

/// days since 1970 in the local time zone on the core's clock, so day-keyed rows turn over with the core's.
nonisolated func coreLocalDay() -> Int { Int(NaviaCoreFFI.localDay()) }

/// A day of the year, such as "November 11", in the user's language. `day` is days since 1970, local.
nonisolated func dayAndMonth(day: Int) -> String {
    let f = DateFormatter()
    f.locale = Locale(identifier: currentLanguage())
    f.timeZone = TimeZone(identifier: "UTC")
    f.setLocalizedDateFormatFromTemplate("MMMMd")
    return f.string(from: Date(timeIntervalSince1970: Double(day) * 86_400))
}
