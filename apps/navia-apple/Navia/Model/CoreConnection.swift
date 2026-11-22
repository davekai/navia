import Foundation
import NaviaCoreFFI

/// Where a login came from. The sign-in screen stores one in the Keychain. Only with `NAVIA_DEV=1`, the
/// environment or a developer's `server.env` takes priority.
enum LoginOrigin: Sendable { case environment, keychain }

struct CoreLogin: Sendable {
    let url: String
    let user: String
    let password: String
    let origin: LoginOrigin

    /// `~/Library/Application Support/Navia/server.env`, mode 600, with NAVIDROME_URL, _USER and _PASSWORD lines.
    /// A developer's login file, read only under `NAVIA_DEV=1` and never changed or imported.
    static var fileURL: URL {
        URL.applicationSupportDirectory.appending(path: "Navia/server.env")
    }

    /// `NAVIA_REPORT_PLAYS=1` starts reporting regardless of Settings, for tests.
    static var reportOverride: Bool { ProcessInfo.processInfo.environment["NAVIA_REPORT_PLAYS"] == "1" }

    var isDeveloperOverride: Bool { origin == .environment }

    var host: String { parseServerAddress(raw: url)?.host ?? url.components(separatedBy: "://").last?.trimmingCharacters(in: CharacterSet(charactersIn: "/")) ?? url }

    /// The login saved by the sign-in screen. Under `NAVIA_DEV=1` the `NAVIDROME_*` environment, then `server.env`,
    /// come first. Nil means first run.
    static func load() -> CoreLogin? {
        if developerOverride() {
            let env = ProcessInfo.processInfo.environment
            func get(_ key: String) -> String? { env[key].flatMap { $0.isEmpty ? nil : $0 } }
            if let url = get("NAVIDROME_URL"), let user = get("NAVIDROME_USER"), let password = get("NAVIDROME_PASSWORD") {
                return CoreLogin(url: url, user: user, password: password, origin: .environment)
            }
            if let file = readFile() {
                return CoreLogin(url: file.url, user: file.user, password: file.password, origin: .environment)
            }
        }
        guard let stored = LoginKeychain.load() else { return nil }
        return CoreLogin(url: stored.url, user: stored.user, password: stored.password, origin: .keychain)
    }

    /// nothing here prints or logs the login.
    private static func readFile() -> LoginKeychain.Stored? {
        guard let text = try? String(contentsOf: fileURL, encoding: .utf8) else { return nil }
        var values: [String: String] = [:]
        for line in text.split(whereSeparator: \.isNewline) {
            guard let eq = line.firstIndex(of: "="), !line.hasPrefix("#") else { continue }
            let key = line[..<eq].trimmingCharacters(in: .whitespaces)
            let value = line[line.index(after: eq)...].trimmingCharacters(in: .whitespaces).trimmingCharacters(in: CharacterSet(charactersIn: "\"'"))
            values[key] = value
        }
        guard let url = values["NAVIDROME_URL"], let user = values["NAVIDROME_USER"], let password = values["NAVIDROME_PASSWORD"],
              !url.isEmpty, !user.isEmpty, !password.isEmpty else { return nil }
        return .init(url: url, user: user, password: password)
    }
}

/// one `NaviaKeychain` secret; nothing here prints or logs its contents.
nonisolated enum LoginKeychain {
    struct Stored: Codable, Sendable { let url, user, password: String }

    private static let account = "login"
    /// The older location, with its own service and account "current".
    private static let legacy = NaviaKeychain.Legacy(service: NaviaKeychain.service + ".login", account: "current")

    static func load() -> Stored? {
        NaviaKeychain.read(account, legacy: legacy).flatMap { try? JSONDecoder().decode(Stored.self, from: $0) }
    }

    /// false if the Keychain refused.
    @discardableResult static func save(_ login: Stored) -> Bool {
        guard let data = try? JSONEncoder().encode(login) else { return false }
        return NaviaKeychain.write(account, data)
    }

    static func delete() { NaviaKeychain.delete(account, legacy: legacy) }

    static var exists: Bool { NaviaKeychain.exists(account, legacy: legacy) }
}

/// App state that belongs to one server and user, such as local stars, Private mode and the player's session.
/// UserDefaults keys and files are namespaced by the core's account key, so accounts never see each other's state.
enum AccountPrefs {
    /// The active account's key, or empty without a login.
    private(set) static var current = ""

    /// Call when a login becomes active. The first account ever signed in takes over state saved before accounts existed.
    static func activate(_ login: CoreLogin?) {
        current = login.map { accountKey(serverUrl: $0.url, username: $0.user) } ?? ""
        if login != nil { migrateLegacyState() }
    }

    /// The `UserDefaults` key for `name` under the active account.
    static func key(_ name: String) -> String { current.isEmpty ? name : "account.\(current).\(name)" }

    /// This account's folder inside the core's data folder. Nil without a login, or in a test run with no data folder.
    static var folder: URL? {
        guard !current.isEmpty else { return nil }
        if let dir = UserDefaults.standard.string(forKey: "naviaDataDir"), !dir.isEmpty {
            return URL(fileURLWithPath: dir).appending(path: current)
        }
        return CoreMode.isTestRun ? nil : URL.applicationSupportDirectory.appending(path: "Navia/\(current)")
    }

    private static func migrateLegacyState() {
        let defaults = UserDefaults.standard
        guard !defaults.bool(forKey: "accountStateMigrated"), !CoreMode.isTestRun else { return }
        defaults.set(true, forKey: "accountStateMigrated")
        defaults.removeObject(forKey: "albumPins") // A stale key nothing reads.
        for name in ["localStars", "privateMode"] {
            if let value = defaults.object(forKey: name) {
                defaults.set(value, forKey: key(name))
                defaults.removeObject(forKey: name)
            }
        }
        let oldSession = URL.applicationSupportDirectory.appending(path: "Navia/session.json")
        if let folder, FileManager.default.fileExists(atPath: oldSession.path), !FileManager.default.fileExists(atPath: folder.appending(path: "session.json").path) {
            try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            try? FileManager.default.moveItem(at: oldSession, to: folder.appending(path: "session.json"))
        }
    }
}

enum CoreMode {
    /// `-naviaSample YES` or `NAVIA_SAMPLE=1`. UI checks use sample data so they never touch the server.
    static var useSample: Bool {
        let defaults = UserDefaults.standard
        return ProcessInfo.processInfo.environment["NAVIA_SAMPLE"] == "1" || defaults.bool(forKey: "naviaSample")
            || (isTestRun && defaults.string(forKey: "naviaCoreCheck") == nil)
    }

    /// A scripted run, with no external lookups, no playlist fetches and no real device unless asked for.
    static var isTestRun: Bool {
        ["naviaCoreCheck", "naviaPages", "naviaDiagnostics", "naviaTranslucencyCheck"].contains { UserDefaults.standard.object(forKey: $0) != nil }
    }

    /// The player's session file, in the account's folder.
    static var sessionFile: URL? { AccountPrefs.folder?.appending(path: "session.json") }
}
