import AppKit
import Foundation
import NaviaCoreFFI

/// Artwork lives in the core's disk cache. Rows point at it with `navia-art://cover/<key>` URLs that
/// `ArtworkLoader` resolves here. A key is an album id or an `ar-`/`pl-` cover id.
@MainActor final class CoreArt {
    static let shared = CoreArt()
    nonisolated static let scheme = "navia-art"

    nonisolated static func url(for key: String) -> URL? {
        guard !key.isEmpty, let encoded = key.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) else { return nil }
        return URL(string: "\(scheme)://cover/\(encoded)")
    }

    private(set) var core: NaviaCore?

    /// Clears everything for a new account, since ids are per server.
    func reset(core: NaviaCore?) {
        self.core = core
        paths.removeAll()
        tints.removeAll()
        loggedFailures.removeAll()
        placeholderUntil.removeAll()
        PlaylistTints.shared.reset()
        pending.removeAll()
        waiting.values.joined().forEach { $0.resume(returning: nil) }
        waiting.removeAll()
        ArtworkLoader.shared.clear()
    }

    /// The core's artwork files were deleted, so paths are asked for again. Tints and decoded images stay.
    func forgetFiles() {
        paths.removeAll()
        placeholderUntil.removeAll()
    }

    /// Dominant color per key, once the core has seen the image.
    private(set) var tints: [String: String] = [:]
    var onTint: (() -> Void)?

    func tint(for url: URL) -> String? { tints[Self.key(of: url)] }

    private nonisolated static func key(of url: URL) -> String {
        let key = url.path.removingPercentEncoding.map { String($0.dropFirst()) } ?? ""
        #if DEBUG
        return alias[key] ?? key
        #else
        return key
        #endif
    }

    #if DEBUG
    /// check runs (`artAlias`).
    nonisolated(unsafe) static var alias: [String: String] = [:]
    #endif

    private var paths: [String: URL] = [:]
    private var waiting: [String: [CheckedContinuation<URL?, Never>]] = [:]
    /// Newest last, so on a fast scroll the rows the user stopped on load first.
    private var pending: [String] = []
    private var running = 0
    private var loggedFailures: Set<String> = []
    /// jobs whose file is the server's no-cover placeholder, and when to ask for them again.
    private var placeholderUntil: [String: Date] = [:]

    /// Parallel requests to the core. The core's own limit is gentler still on the server.
    private let concurrency = 8
    /// Past this the oldest requests are dropped. Those rows ask again when they scroll back.
    private let backlog = 256

    private init() {}

    /// The core keeps a few fixed sizes so Navidrome caches each one once.
    private static func size(for pixels: Int) -> UInt32? { [128, 256, 512].first { $0 >= pixels }.map(UInt32.init) }

    private static func job(for url: URL, pixels: Int) -> String { "\(size(for: pixels).map(String.init) ?? "orig")@\(key(of: url))" }

    /// the file last given for this art is the server's no-cover placeholder, so it isn't kept decoded.
    func isPlaceholder(_ url: URL, pixels: Int) -> Bool { placeholderUntil[Self.job(for: url, pixels: pixels)] != nil }

    func file(for url: URL, pixels: Int) async -> URL? {
        let job = Self.job(for: url, pixels: pixels)
        // asked for again once it expires, in case a cover was added since.
        if let until = placeholderUntil[job], until <= Date() {
            paths[job] = nil
            placeholderUntil[job] = nil
        }
        // the core may have trimmed or lost the file since; then it fetches again.
        if let hit = paths[job] {
            if FileManager.default.fileExists(atPath: hit.path) { return hit }
            paths[job] = nil
        }
        return await withCheckedContinuation { (cont: CheckedContinuation<URL?, Never>) in
            if waiting[job] != nil {
                waiting[job]?.append(cont)
                return
            }
            waiting[job] = [cont]
            pending.append(job)
            while pending.count > backlog {
                let old = pending.removeFirst()
                waiting.removeValue(forKey: old)?.forEach { $0.resume(returning: nil) }
            }
            pump()
        }
    }

    private func pump() {
        guard let core else {
            pending.removeAll()
            let stale = waiting
            waiting.removeAll()
            stale.values.joined().forEach { $0.resume(returning: nil) }
            return
        }
        while running < concurrency, let job = pending.popLast() {
            running += 1
            let parts = job.split(separator: "@", maxSplits: 1)
            let size = UInt32(parts[0])
            let key = parts.count > 1 ? String(parts[1]) : ""
            Task.detached(priority: .utility) {
                var cover: CoverArtRow?
                var failure: String?
                do {
                    cover = try await core.cover(key: key, size: size)
                } catch {
                    failure = String(describing: error)
                }
                await MainActor.run { [cover, failure] in
                    if let failure { self.logFailure(key: key, failure) }
                    self.finished(job, key: key, cover: cover)
                }
            }
        }
    }

    /// once per key and run, so a cover that keeps failing doesn't flood the log while rows scroll.
    private func logFailure(key: String, _ failure: String) {
        guard loggedFailures.insert(key).inserted else { return }
        Log.library.error("art failed for \(key, privacy: .public): \(failure, privacy: .public)")
    }

    /// animated covers are asked for at these sizes only, so each cover keeps a few cached files.
    private static func animatedSize(for pixels: Int) -> UInt32 { UInt32([256, 512, 1024].first { $0 >= pixels } ?? 1024) }

    /// frames shared by every view showing the same cover at the same size, dropped once none shows it.
    private let animated = NSMapTable<NSString, AnimatedFrames>.strongToWeakObjects()

    /// ask only where art is large.
    func animatedFrames(for url: URL, pixels: Int) async -> AnimatedFrames? {
        guard let core, url.scheme == Self.scheme else { return nil }
        let key = Self.key(of: url)
        let size = Self.animatedSize(for: pixels)
        let job = "\(size)@\(key)" as NSString
        if let hit = animated.object(forKey: job) { return hit }
        if core.coverIsAnimated(key: key) == false { return nil }
        let frames: AnimatedFrames? = await Task.detached(priority: .utility) {
            guard let art = try? await core.animatedArtwork(key: key, size: size) else { return nil }
            return AnimatedFrames(art)
        }.value
        if let frames { animated.setObject(frames, forKey: job) }
        return frames
    }

    private func finished(_ job: String, key: String, cover: CoverArtRow?) {
        running -= 1
        var url: URL?
        if let cover {
            url = URL(fileURLWithPath: cover.path)
            paths[job] = url
            placeholderUntil[job] = cover.placeholder ? Date().addingTimeInterval(TimeInterval(placeholderTtlSeconds())) : nil
            let bytes = cover.tintRgb.prefix(3).map { String(format: "%02x", $0) }.joined()
            if tints[key] != bytes {
                tints[key] = bytes
                onTint?()
            }
        }
        waiting.removeValue(forKey: job)?.forEach { $0.resume(returning: url) }
        pump()
    }
}

nonisolated extension SongRow {
    init(core s: NaviaCoreFFI.SongRow) {
        let albumID = s.albumId ?? ""
        self.init(id: s.id, title: s.title, artist: s.artist ?? "", album: s.album ?? "", albumID: albumID,
                  duration: Int(s.durationSeconds ?? 0), genre: s.genre ?? "", year: s.year.map(Int.init), plays: Int(s.playCount ?? 0),
                  isFavorite: s.starred, isDownloaded: false, artworkURL: CoreArt.url(for: albumID), tintHex: nil,
                  trackNumber: s.track.map(Int.init), hasLyrics: false,
                  isHiRes: (s.bitDepth ?? 0) > 16 || (s.samplingRate ?? 0) > 48_000,
                  rowid: s.rowid,
                  credits: s.artists.map { FeaturedArtist(name: $0.name, photoURL: CoreArt.url(for: "ar-\($0.id)"), artistID: $0.id) },
                  disc: s.disc.map(Int.init), isExplicit: s.explicit, added: s.created)
    }
}

nonisolated extension AlbumRow {
    init(core a: NaviaCoreFFI.AlbumRow) {
        self.init(id: a.id, name: a.name, artist: a.artist ?? "", artistID: a.artistId, year: a.year.map(Int.init),
                  songCount: Int(a.songCount ?? 0), duration: Int(a.durationSeconds ?? 0), genre: a.genre ?? "",
                  artworkURL: CoreArt.url(for: a.id), tintHex: nil, blurb: "", isFavorite: a.starred,
                  isExplicit: a.explicit, discTitles: Dictionary(a.discTitles.map { (Int($0.disc), $0.title) }, uniquingKeysWith: { a, _ in a }),
                  credits: a.artists.map { FeaturedArtist(name: $0.name, photoURL: CoreArt.url(for: "ar-\($0.id)"), artistID: $0.id) },
                  edition: a.edition, isNewRelease: a.newRelease)
    }
}

nonisolated extension UpcomingAlbum {
    init(core a: UpcomingAlbumRow) {
        var unavailable: Set<String> = []
        let tracks = a.tracks.enumerated().map { place, t in
            var song = t.song.map { SongRow(core: $0) } ?? {
                let id = "dz-\(t.id)"
                unavailable.insert(id)
                return SongRow(id: id, title: t.title, artist: t.artist, album: a.title, albumID: "", duration: Int(t.durationSeconds), genre: "", year: nil,
                               plays: 0, isFavorite: false, isDownloaded: false, artworkURL: nil, tintHex: nil, isExplicit: t.explicit)
            }()
            song.trackNumber = place + 1
            return song
        }
        self.init(id: a.id, title: a.title, artist: a.artist, artistID: a.artistId, coverURL: a.coverUrl.flatMap(URL.init(string:)), releaseDate: a.releaseDate,
                  releaseMoment: Date(timeIntervalSince1970: TimeInterval(a.releaseMoment)), total: Int(a.total), isExplicit: a.explicit, tracks: tracks, unavailable: unavailable)
    }
}

nonisolated extension ArtistRow {
    init(core a: NaviaCoreFFI.ArtistRow, bio: String = "") {
        self.init(id: a.id, name: a.name, photoURL: CoreArt.url(for: "ar-\(a.id)"), tintHex: nil, bio: bio, isFavorite: a.starred)
    }
}

func residentMemoryBytes() -> UInt64 {
    var info = mach_task_basic_info()
    var count = mach_msg_type_number_t(MemoryLayout<mach_task_basic_info>.size / MemoryLayout<natural_t>.size)
    let result = withUnsafeMutablePointer(to: &info) { $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) { task_info(mach_task_self_, task_flavor_t(MACH_TASK_BASIC_INFO), $0, &count) } }
    return result == KERN_SUCCESS ? UInt64(info.resident_size) : 0
}
