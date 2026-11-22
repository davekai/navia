import Foundation
import NaviaCoreFFI
import Observation

/// The sign-in form, on first run and in Settings > Change Server. Test and Sign In each make one read-only ping
/// and report the result under the field it's about. The first sign-in is followed by the one-time Deezer step.
@Observable final class ConnectModel {
    enum Mode { case firstRun, change }
    /// Which part of the form a problem is about. Its message sits under that field and rings it.
    enum Field { case address, account, form }
    enum Outcome: Equatable {
        /// `test` is true for Test, false for Sign In.
        case checking(test: Bool)
        case connected
        case found
        case bad(field: Field, title: String, detail: String)
    }

    let mode: Mode
    var address: String { didSet { if address != oldValue { outcome = nil; serverLine = nil } } }
    var user: String { didSet { if user != oldValue { outcome = nil } } }
    var password = "" { didSet { if password != oldValue { outcome = nil } } }
    var reveal = false
    private(set) var outcome: Outcome?
    /// "<server> · <API>" once a check found the server, until the address changes.
    private(set) var serverLine: String?
    /// shown with the account already open behind it.
    var deezerStep = false
    /// The saved login was refused. Cancel reads Browse Offline, since the app keeps what's on this device.
    private(set) var rejected = false

    init(mode: Mode, address: String = "", user: String = "") {
        self.mode = mode
        self.address = address
        self.user = user
    }

    /// Notes under the account fields that the server refused the saved login.
    func noteRejected() {
        rejected = true
        outcome = .bad(field: .account, title: L("Wrong username or password"), detail: L("The server rejected the saved login (error 40). Sign in again, or keep browsing what's on this device."))
    }

    var isChecking: Bool { outcome == .checking(test: true) || outcome == .checking(test: false) }
    var isTesting: Bool { outcome == .checking(test: true) }
    var isSigningIn: Bool { outcome == .checking(test: false) }
    func problem(_ field: Field) -> (title: String, detail: String)? {
        if case let .bad(f, title, detail) = outcome, f == field { return (title, detail) }
        return nil
    }
    var isComplete: Bool { !address.trimmingCharacters(in: .whitespaces).isEmpty && !user.trimmingCharacters(in: .whitespaces).isEmpty && !password.isEmpty }
    /// Plain HTTP to a host outside the local network. A warning, not a block.
    var isInsecure: Bool { parseServerAddress(raw: address)?.unencrypted ?? false }
    var canSubmit: Bool { isComplete && !isChecking }
    var title: String { mode == .change ? L("Change Server") : L("Sign in to a server") }
    var submitTitle: String { isSigningIn ? L("Signing In…") : mode == .change ? L("Save and Reconnect") : L("Sign In") }

    /// true when the server accepted this login.
    @discardableResult func test() async -> Bool { await check(test: true) != nil }

    /// Checks, and on success signs in through `shell`, which closes this form or moves on to the Deezer step.
    func connect(shell: ShellModel) async {
        guard canSubmit, let address = await check(test: false) else { return }
        shell.signIn(address: address, user: user.trimmingCharacters(in: .whitespaces), password: password)
    }

    /// The normalized address when the server accepted the login, or nil. Sets `outcome` either way.
    private func check(test: Bool) async -> String? {
        let bad = { (field: Field, title: String, detail: String) in self.outcome = .bad(field: field, title: title, detail: detail) }
        guard let parsed = parseServerAddress(raw: address) else {
            bad(.address, L("That isn't a web address"), L("Use the full address, like https://music.example.org or http://192.168.1.20:4533."))
            return nil
        }
        outcome = .checking(test: test)
        let result = await probeServer(address: parsed.url, username: user.trimmingCharacters(in: .whitespaces), password: password)
        // The form may have been edited while the check ran, which makes this result stale.
        guard parseServerAddress(raw: address)?.url == parsed.url else { outcome = nil; return nil }
        switch result {
        case let .connected(server, api, _):
            serverLine = "\(server) · \(api)"
            outcome = .connected
            return parsed.url
        case let .found(server, api):
            serverLine = "\(server) · \(api)"
            outcome = .found
        case .badAddress:
            bad(.address, L("That isn't a web address"), L("Use the full address, like https://music.example.org or http://192.168.1.20:4533."))
        case .refused:
            bad(.address, L("Can't reach this server"), L("The connection was refused. Check the address and port, and that the server is running."))
        case .timedOut:
            bad(.address, L("Can't reach this server"), L("It did not answer within 10 seconds. It may be down, or a firewall may be in the way."))
        case .unknownHost:
            bad(.address, L("No server found at this name"), L("The name could not be resolved. Check the spelling, or your DNS or VPN."))
        case .badCertificate:
            bad(.address, L("This server's certificate isn't valid"), L("The certificate is expired or doesn't match this name, so the connection was not made."))
        case .notSecure:
            bad(.address, L("Couldn't connect securely"), L("A secure connection couldn't be made. If this server uses plain HTTP, start the address with http://."))
        case let .unreachable(detail):
            bad(.address, L("Can't reach this server"), localizedUnreachable(detail))
        case .wrongLogin:
            bad(.account, L("Wrong username or password"), L("The server rejected this login (error 40)."))
        case .tokenUnsupported:
            bad(.account, L("This server won't accept this kind of sign-in"), L("Navia signs in with Subsonic tokens, and this server refused them (error 41)."))
        case let .notNavidrome(name):
            bad(.address, L("This is not a Navidrome server"), L("It answered as {name}. Navia only works with Navidrome.", ["name": name]))
        case .notSubsonic:
            bad(.address, L("This address didn't answer like a server"), L("A web page came back, not the Subsonic API. If it sits behind a proxy or login page, point to Navidrome itself."))
        case let .serverError(code, message):
            bad(.form, L("The server returned an error"), message.isEmpty ? L("Error {code}.", ["code": String(code)]) : L("{message} (error {code}).", ["message": message, "code": String(code)]))
        }
        return nil
    }

    /// The core's fixed "unreachable" details in the app's language. One of them carries a status code.
    private func localizedUnreachable(_ detail: String) -> String {
        let prefix = "The server answered with HTTP "
        if detail.hasPrefix(prefix), detail.hasSuffix(".") {
            let status = String(detail.dropFirst(prefix.count).dropLast())
            return L("The server answered with HTTP {status}.", ["status": status])
        }
        return L(detail)
    }
}
