import Foundation
import Security

/// Every secret Navia keeps on the Mac, as generic-password items in the login keychain under one service.
/// Nothing here prints or logs a value.
///
/// The service is the bundle id, so a build with another id never touches the real app's items. A login keychain
/// item trusts only the app that created it, so another app reading it prompts, and rewriting it leaves an item
/// the real app has to ask for on every launch.
nonisolated enum NaviaKeychain {
    static let service = Bundle.main.bundleIdentifier ?? "com.davekai.Navia"

    /// An account still kept under the previous bundle id is treated like a legacy item; check builds leave them alone.
    private static func older(_ account: String, _ legacy: Legacy?) -> [Legacy] {
        guard !service.hasPrefix(BundleMove.previousID) else { return [] }
        return [Legacy(service: BundleMove.previousID, account: account), legacy].compactMap { $0 }
    }

    /// Where a secret was kept before the shared service.
    struct Legacy { let service: String; let account: String }

    private static func query(service: String, account: String) -> [CFString: Any] {
        [kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: account]
    }

    private static func copy(service: String, account: String) -> Data? {
        var q = query(service: service, account: account)
        q[kSecReturnData] = true
        q[kSecMatchLimit] = kSecMatchLimitOne
        var out: CFTypeRef?
        guard SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess else { return nil }
        return out as? Data
    }

    private static func present(service: String, account: String) -> Bool {
        var q = query(service: service, account: account)
        q[kSecMatchLimit] = kSecMatchLimitOne
        // Reads attributes only, so this never prompts.
        return SecItemCopyMatching(q as CFDictionary, nil) == errSecSuccess
    }

    /// The secret, or nil. An item still at an older place is moved here once, saved as this app's own.
    static func read(_ account: String, legacy: Legacy? = nil) -> Data? {
        if let data = copy(service: service, account: account) { return data }
        for old in older(account, legacy) {
            guard let data = copy(service: old.service, account: old.account) else { continue }
            if write(account, data) {
                SecItemDelete(query(service: old.service, account: old.account) as CFDictionary)
            }
            return data
        }
        return nil
    }

    /// false when the Keychain refused.
    @discardableResult static func write(_ account: String, _ data: Data) -> Bool {
        SecItemDelete(query(service: service, account: account) as CFDictionary)
        var q = query(service: service, account: account)
        q[kSecValueData] = data
        q[kSecAttrLabel] = "Navia"
        return SecItemAdd(q as CFDictionary, nil) == errSecSuccess
    }

    /// Removes the secret and any older item left. True when nothing is left.
    @discardableResult static func delete(_ account: String, legacy: Legacy? = nil) -> Bool {
        for old in older(account, legacy) {
            SecItemDelete(query(service: old.service, account: old.account) as CFDictionary)
        }
        let status = SecItemDelete(query(service: service, account: account) as CFDictionary)
        return status == errSecSuccess || status == errSecItemNotFound
    }

    static func exists(_ account: String, legacy: Legacy? = nil) -> Bool {
        present(service: service, account: account)
            || older(account, legacy).contains { present(service: $0.service, account: $0.account) }
    }
}
