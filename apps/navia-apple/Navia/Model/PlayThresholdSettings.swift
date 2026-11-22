import Foundation
import NaviaCoreFFI
import Observation

/// Settings > Scrobble plays: when a play counts. Saved with the account in the core, and applied to the current
/// song if it hasn't counted yet.
@MainActor @Observable final class PlayThresholdSettings {
    @ObservationIgnored private let core: NaviaCore
    /// The percentages Settings offers.
    let choices: [UInt32]
    private(set) var current: PlayThresholdRow

    init(core: NaviaCore) {
        self.core = core
        choices = core.playThresholdChoices()
        current = core.playThreshold()
    }

    func setPercent(_ percent: UInt32) { save(PlayThresholdRow(percent: percent, capAtFourMinutes: current.capAtFourMinutes)) }

    func setCap(_ on: Bool) { save(PlayThresholdRow(percent: current.percent, capAtFourMinutes: on)) }

    private func save(_ next: PlayThresholdRow) {
        current = next
        let core = core
        Task.detached(priority: .utility) { try? core.setPlayThreshold(threshold: next) }
    }

    static func percentLabel(_ percent: UInt32) -> String { L("{percent}%", ["percent": String(percent)]) }
}
