import Foundation
import Observation
import ServiceManagement

/// Settings > Open at login, through `SMAppService.mainApp`. The system's state is the setting, so changes in
/// System Settings show here too.
@MainActor @Observable final class OpenAtLogin {
    private(set) var isOn = false
    /// registered, but waiting for the user to allow it in System Settings.
    private(set) var needsApproval = false

    init() { refresh() }

    func refresh() {
        let status = SMAppService.mainApp.status
        isOn = status == .enabled || status == .requiresApproval
        needsApproval = status == .requiresApproval
    }

    func set(_ on: Bool) {
        do {
            if on { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
        } catch {
            NSLog("navia: open at login: \(error.localizedDescription)")
        }
        refresh()
    }
}
