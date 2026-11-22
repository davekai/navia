import Foundation
import NaviaCoreFFI
import Observation

/// Settings > Storage, covering the song cache's limit, each cache's size on disk, and clearing them.
/// Everything here touches files, so it runs off the main thread.
@MainActor @Observable final class StorageSettings {
    @ObservationIgnored private let core: NaviaCore
    /// Limits Settings offers, in bytes. 0 is off.
    let choices: [UInt64]
    private(set) var limit: UInt64
    private(set) var usage: StorageUsage?
    @ObservationIgnored private var loading = false
    /// Runs after the caches are cleared, so remembered file paths are dropped.
    @ObservationIgnored var onCleared: (() -> Void)?
    /// Runs after cached songs are removed, so rows that play offline are read again.
    @ObservationIgnored var onSongsChanged: (() -> Void)?

    init(core: NaviaCore) {
        self.core = core
        choices = core.songCacheLimitChoices()
        limit = core.songCacheLimit()
    }

    func refresh() {
        guard !loading else { return }
        loading = true
        let core = core
        Task {
            let measured = await Task.detached(priority: .utility) { core.storageUsage() }.value
            usage = measured
            limit = measured.songCacheLimit
            loading = false
        }
    }

    func setLimit(_ bytes: UInt64) {
        limit = bytes
        let core = core
        Task {
            // A lower limit deletes files at once.
            await Task.detached(priority: .utility) { try? core.setSongCacheLimit(bytes: bytes) }.value
            refresh()
        }
    }

    /// The songs in the song cache, most recent first. Only complete files are kept. A song the library no longer
    /// lists still shows, so it can be removed.
    func cachedSongs() async -> [CachedSong] {
        let core = core
        return await Task.detached(priority: .userInitiated) {
            let rows = core.cachedSongs()
            var known: [String: SongRow] = [:]
            for row in core.songsById(ids: rows.map(\.id)).map({ SongRow(core: $0) }) { known[row.id] = row }
            return rows.map { row in
                CachedSong(song: known[row.id] ?? SongRow(id: row.id, title: L("Unknown song"), artist: L("No longer on the server"), album: "", albumID: "", duration: 0, genre: "", year: nil, plays: 0, isFavorite: false, isDownloaded: false, artworkURL: nil, tintHex: nil),
                           inLibrary: known[row.id] != nil, bytes: Int64(row.bytes), playedMs: row.lastPlayedMs)
            }
        }.value
    }

    func removeCached(_ ids: [String]) async {
        let core = core
        await Task.detached(priority: .userInitiated) { _ = core.removeCachedSongs(ids: ids) }.value
        onSongsChanged?()
        refresh()
    }

    /// Removes every cached song, but not artwork, lyrics or downloads.
    func clearSongs() async {
        let core = core
        await Task.detached(priority: .userInitiated) { core.clearSongCache() }.value
        onSongsChanged?()
        refresh()
    }

    /// Downloads and the library stay.
    func clear() {
        let core = core
        Task {
            await Task.detached(priority: .userInitiated) { core.clearCaches() }.value
            onCleared?()
            refresh()
        }
    }

    /// The limit in whole gigabytes, such as `2 GB`, or Off for 0.
    static func limitLabel(_ bytes: UInt64) -> String {
        bytes == 0 ? L("Off") : "\(fmtInt(Int(bytes / 1_000_000_000))) GB"
    }

    /// Sizes as Downloads shows them, with zero as 0 KB.
    static func sizeLabel(_ bytes: UInt64) -> String {
        bytes == 0 ? "\(fmtInt(0)) KB" : formatSize(Int64(bytes))
    }
}

/// A song in the song cache, with the total size of its copies and when it was last played or cached.
nonisolated struct CachedSong: Sendable {
    let song: SongRow
    /// False when the library no longer lists it, so it can only be removed.
    let inLibrary: Bool
    let bytes: Int64
    let playedMs: Int64
}
