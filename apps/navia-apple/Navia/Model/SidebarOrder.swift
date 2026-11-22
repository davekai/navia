import Foundation

/// The sidebar's playlist order. Navidrome has no playlist order, so it's stored on the device per server and user,
/// keyed by playlist id so renames keep their place.
nonisolated enum SidebarOrder {
    /// `current` reordered, with everything in `saved` that still exists in that order, then anything new.
    static func merged(saved: [String], current: [PlaylistRef]) -> [PlaylistRef] {
        var byID = Dictionary(current.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        var out: [PlaylistRef] = []
        for id in saved {
            if let row = byID.removeValue(forKey: id) { out.append(row) }
        }
        return out + current.filter { byID[$0.id] != nil }
    }

    /// `gap` 0 is before the first, `count` after the last. false when that changes nothing.
    @discardableResult static func move<T>(_ items: inout [T], from: Int, toGap gap: Int) -> Bool {
        guard items.indices.contains(from), (0...items.count).contains(gap), gap != from, gap != from + 1 else { return false }
        let item = items.remove(at: from)
        items.insert(item, at: gap > from ? gap - 1 : gap)
        return true
    }

    private static func key(_ account: String) -> String { "sidebarPlaylistOrder.\(account)" }

    static func load(account: String) -> [String] { UserDefaults.standard.stringArray(forKey: key(account)) ?? [] }
    static func save(_ ids: [String], account: String) { UserDefaults.standard.set(ids, forKey: key(account)) }
}

/// Navidrome has no pins either, so they stay on the device per account. own playlists start pinned and others' don't;
/// only the exceptions are kept.
nonisolated struct SidebarPins: Codable, Equatable {
    var pinned: [String] = []
    var unpinned: [String] = []

    func isPinned(_ row: PlaylistRef) -> Bool {
        if pinned.contains(row.id) { return true }
        if unpinned.contains(row.id) { return false }
        return row.section == .mine
    }

    mutating func set(_ row: PlaylistRef, on: Bool) {
        pinned.removeAll { $0 == row.id }
        unpinned.removeAll { $0 == row.id }
        if on != (row.section == .mine) {
            if on { pinned.append(row.id) } else { unpinned.append(row.id) }
        }
    }

    private static func key(_ account: String) -> String { "sidebarPins.\(account)" }

    static func load(account: String) -> SidebarPins {
        guard let data = UserDefaults.standard.data(forKey: key(account)) else { return SidebarPins() }
        return (try? JSONDecoder().decode(SidebarPins.self, from: data)) ?? SidebarPins()
    }

    func save(account: String) {
        if let data = try? JSONEncoder().encode(self) { UserDefaults.standard.set(data, forKey: Self.key(account)) }
    }
}
