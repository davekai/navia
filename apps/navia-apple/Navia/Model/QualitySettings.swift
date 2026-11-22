import Foundation
import Network
import NaviaCoreFFI
import Observation

/// Streaming, metered and download quality, plus volume leveling, stored in the account's library. Metered comes
/// from `NWPathMonitor`, since a hotspot looks like plain Wi-Fi.
@MainActor @Observable final class QualitySettings {
    enum Slot { case streaming, metered, download }

    @ObservationIgnored private let core: NaviaCore
    @ObservationIgnored private let monitor = NWPathMonitor()
    @ObservationIgnored private let queue = DispatchQueue(label: "navia.metered", qos: .utility)

    let formats: [QualityFormat]
    private(set) var streaming: QualityChoice
    private(set) var metered: QualityChoice
    private(set) var download: QualityChoice
    private(set) var leveling: String
    private(set) var connectionMetered = false
    private(set) var unavailable: Set<String> = []
    @ObservationIgnored private var checkedServer = false

    init(core: NaviaCore) {
        self.core = core
        formats = core.qualityFormats()
        streaming = core.streamingQuality()
        metered = core.meteredQuality()
        download = core.downloadQuality()
        leveling = core.leveling()
        let target = core
        monitor.pathUpdateHandler = { [weak self] path in
            let isMetered = path.isExpensive || path.isConstrained
            // The core picks the matching quality for the next track.
            target.setConnectionMetered(metered: isMetered)
            Task { @MainActor [weak self] in self?.connectionMetered = isMetered }
        }
        monitor.start(queue: queue)
    }

    deinit { monitor.cancel() }

    func choice(_ slot: Slot) -> QualityChoice {
        switch slot {
        case .streaming: streaming
        case .metered: metered
        case .download: download
        }
    }

    func bitrates(format: String) -> [UInt32] { formats.first { $0.id == format }?.bitrates ?? [] }
    func defaultKbps(format: String) -> UInt32 { formats.first { $0.id == format }?.defaultKbps ?? 0 }

    func set(_ slot: Slot, format: String, kbps: UInt32) {
        do {
            switch slot {
            case .streaming: try core.setStreamingQuality(format: format, kbps: kbps); streaming = core.streamingQuality()
            case .metered: try core.setMeteredQuality(format: format, kbps: kbps); metered = core.meteredQuality()
            case .download: try core.setDownloadQuality(format: format, kbps: kbps); download = core.downloadQuality()
            }
        } catch {
            Log.app.error("couldn't save the quality setting: \(String(describing: error), privacy: .public)")
        }
    }

    func setLeveling(_ mode: String) {
        do {
            try core.setLeveling(mode: mode)
            leveling = core.leveling()
        } catch {
            Log.app.error("couldn't save volume leveling: \(String(describing: error), privacy: .public)")
        }
    }

    /// once, off the main thread.
    func checkServerOnce() {
        guard !checkedServer else { return }
        checkedServer = true
        let core = self.core
        Task.detached(priority: .utility) { [weak self] in
            let found = Set(core.unavailableFormats())
            await MainActor.run { self?.unavailable = found }
        }
    }
}
