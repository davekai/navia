import Foundation
import NaviaCoreFFI
import Observation

private nonisolated final class CollectionEvents: CollectionListener, @unchecked Sendable {
    let handler: @Sendable () -> Void
    init(_ handler: @escaping @Sendable () -> Void) { self.handler = handler }
    func onCollectionChanged(kind: CollectionKind, id: String) { handler() }
}

/// Playlists and albums kept on this device and in sync with the server. The core keeps them, and this is the app's
/// copy of their status, refreshed when the core reports a change.
@Observable final class SyncedDownloads {
    struct Row: Identifiable {
        let kind: CollectionKind
        let collectionID: String
        let status: CollectionStatusRow
        var id: String { "\(kind == .playlist ? "playlist" : "album"):\(collectionID)" }
    }

    private(set) var rows: [Row] = []
    private(set) var revision = 0
    @ObservationIgnored private let core: NaviaCore
    @ObservationIgnored private var refreshing = false
    @ObservationIgnored private var again = false

    init(core: NaviaCore) {
        self.core = core
        core.setCollectionListener(listener: CollectionEvents { [weak self] in
            Task { @MainActor in self?.refresh() }
        })
        refresh()
    }

    func status(_ kind: CollectionKind, _ id: String) -> CollectionStatusRow? {
        rows.first { $0.kind == kind && $0.collectionID == id }?.status
    }

    /// Reads every kept collection's status off the main thread. Events during a read queue one more.
    func refresh() {
        guard !refreshing else { again = true; return }
        refreshing = true
        let core = core
        Task { [weak self] in
            let read = await Task.detached(priority: .utility) {
                core.syncedCollections().compactMap { c in core.collectionStatus(kind: c.kind, id: c.id).map { Row(kind: c.kind, collectionID: c.id, status: $0) } }
            }.value
            guard let self else { return }
            self.rows = read
            self.revision &+= 1
            self.refreshing = false
            if self.again { self.again = false; self.refresh() }
        }
    }

    /// Turns the switch on. The core fetches a playlist's songs first when this device has none.
    func turnOn(_ kind: CollectionKind, _ id: String) {
        let core = core
        Task.detached(priority: .userInitiated) { try? await core.downloadCollection(kind: kind, id: id) }
    }

    /// What turning it off would remove, and what stays because something else holds it.
    func removalPlan(_ kind: CollectionKind, _ id: String) -> RemovalPlanRow { core.collectionRemovalPlan(kind: kind, id: id) }

    /// Keep Songs leaves them as single downloads, and Remove deletes what nothing else holds.
    func turnOff(_ kind: CollectionKind, _ id: String, keepSongs: Bool) {
        let core = core
        Task.detached(priority: .utility) { try? core.stopCollectionDownload(kind: kind, id: id, keepSongs: keepSongs) }
    }

    func singleIDs() -> Set<String> { Set(core.singleDownloadIds()) }

    /// Clear All removes every download and stops kept collections.
    func clearAll() {
        let core = core
        Task.detached(priority: .utility) { core.clearAllDownloads() }
    }

    /// Settings > Library > Download on metered connections. Off by default, per account.
    var downloadOnMetered: Bool {
        get { _ = revision; return core.downloadOnMetered() }
        set { try? core.setDownloadOnMetered(on: newValue); revision &+= 1 }
    }

    /// A status's glyph, filled when done, gray while waiting, and a filling ring otherwise.
    static func glyph(_ s: CollectionStatusRow) -> DownloadGlyph.Kind {
        switch s.phase {
        case .done: .done
        case .metered, .offline, .waiting: .paused
        case .downloading, .syncing: .running
        }
    }

    static func statusText(_ s: CollectionStatusRow) -> String {
        let progress = L("{have} of {total}", ["have": fmtInt(Int(s.have)), "total": fmtInt(Int(s.total))])
        switch s.phase {
        case .done: return L("Downloaded")
        case .syncing: return Ln("Downloading {count} new song", "Downloading {count} new songs", Int(s.missing))
        case .downloading: return L("Downloading {progress}", ["progress": progress])
        case .metered: return L("Paused on a metered connection, {progress}", ["progress": progress])
        case .offline: return L("Waiting for the server, {progress}", ["progress": progress])
        case .waiting: return L("Waiting to download")
        }
    }
}
