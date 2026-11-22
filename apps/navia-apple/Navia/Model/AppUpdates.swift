import AppKit
import Foundation
import NaviaCoreFFI
import Observation

/// Settings > About and Check for Updates. Until the Mac has Developer ID signing and its own updater, a check asks
/// GitHub for the latest release and Download opens its page.
@MainActor @Observable final class AppUpdates {
    static let shared = AppUpdates()

    enum Status: Equatable { case idle, checking, upToDate, available(version: String, url: String), failed }

    private(set) var status: Status = .idle

    var checkAutomatically: Bool {
        didSet { UserDefaults.standard.set(checkAutomatically, forKey: "checkUpdates") }
    }

    private var automaticChecks: Task<Void, Never>?

    private init() {
        checkAutomatically = UserDefaults.standard.object(forKey: "checkUpdates") as? Bool ?? true
    }

    /// "0.1.0+543 (f6e8534)" for a test build, "0.1.0" for a release. The version is stamped from the workspace
    /// Cargo.toml by `tools/version.sh xcode`, and the commit from `NaviaCommit`, empty for a release.
    static var versionLabel: String {
        let info = Bundle.main.infoDictionary ?? [:]
        let version = info["CFBundleShortVersionString"] as? String ?? "?"
        let commit = (info["NaviaCommit"] as? String ?? "").trimmingCharacters(in: .whitespaces)
        let build = info["CFBundleVersion"] as? String ?? ""
        if commit.isEmpty { return version }
        return build.isEmpty ? "\(version) (\(commit))" : "\(version)+\(build) (\(commit))"
    }

    /// The release version for the core's User-Agent, never a test build's number, since Navidrome makes a new player
    /// for every new agent.
    nonisolated static var versionForAgent: String {
        Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "?"
    }

    var note: String? {
        switch status {
        case .idle: nil
        case .checking: L("Checking…")
        case .upToDate: L("Navia is up to date")
        case .available(let version, _): L("Navia {version} is available", ["version": version])
        case .failed: L("Couldn't check for updates")
        }
    }

    var buttonTitle: String {
        switch status {
        case .checking: L("Checking…")
        case .available: L("Download")
        default: L("Check for Updates")
        }
    }

    /// Checks, or opens the newer release's page.
    func check() {
        switch status {
        case .checking: return
        case .available(_, let url):
            if let link = URL(string: url) { NSWorkspace.shared.open(link) }
        default:
            run(automatic: false)
        }
    }

    /// At launch and then every few hours, never in test runs, sample mode or the developer override.
    func startAutomaticChecks() {
        guard automaticChecks == nil, automaticUpdateCheckAllowed(testRun: CoreMode.isTestRun || CoreMode.useSample) else { return }
        let interval = Duration.seconds(Int64(updateCheckIntervalSecs()))
        automaticChecks = Task {
            while !Task.isCancelled {
                if checkAutomatically, status != .checking { run(automatic: true) }
                try? await Task.sleep(for: interval)
            }
        }
    }

    private func run(automatic: Bool) {
        if !automatic { status = .checking }
        let running = Self.versionForAgent
        Task.detached(priority: .utility) {
            let answer = checkLatestRelease(runningVersion: running)
            await MainActor.run { self.finish(answer, automatic: automatic) }
        }
    }

    private func finish(_ answer: ReleaseCheckRow, automatic: Bool) {
        let next: Status
        switch answer {
        case .upToDate: next = .upToDate
        case .available(let version, let url): next = .available(version: version, url: url)
        case .failed: next = .failed
        }
        if !automatic {
            status = next
        } else if case .available = next, status != next, status != .checking {
            status = next
        }
    }
}
