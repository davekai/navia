import Foundation
import NaviaCoreFFI

// The smart playlist editor's logic lives in the core and is shared with GPUI. This is the app's side, with its
// words for the text the core writes and the field catalog in Swift form.

/// The app's catalog and playlist names, for the text the core composes.
nonisolated final class SmartText: SmartWords, @unchecked Sendable {
    private let names: [String: String]

    init(playlists: [(id: String, name: String)] = []) {
        names = Dictionary(playlists.map { ($0.id, $0.name) }, uniquingKeysWith: { a, _ in a })
    }

    func text(key: String) -> String { L(key) }
    func plural(one: String, other: String, n: UInt64) -> String { pluralTemplate(one, other, Int(n)) }
    func number(n: Int64) -> String { fmtInt(Int(n)) }
    func playlistName(id: String) -> String? { names[id] }
}

/// A saved playlist's rules in words, from the core.
nonisolated func smartRuleSummary(_ raw: String?, playlists: [(id: String, name: String)]) -> RuleSummary? {
    guard let raw, let s = smartSummary(criteria: raw, words: SmartText(playlists: playlists)) else { return nil }
    return RuleSummary(sentence: s.sentence, match: s.matchWord, rules: s.rules, order: s.order)
}

/// read once from the core.
nonisolated let smartFieldList: [SmartFieldRow] = smartFields()
nonisolated let smartSortRandom = "random"
