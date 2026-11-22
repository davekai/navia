import Foundation
import NaviaCoreFFI
import Observation

/// The user's share links, and creating, editing and deleting them. Without a `core` there is no sharing.
@MainActor @Observable final class ShareLinks {
    @ObservationIgnored private let core: NaviaCore?
    private(set) var links: [ShareLinkRow]?
    private(set) var problem: String?

    init(core: NaviaCore?) { self.core = core }

    var sharingOn: Bool { core?.sharing()?.on ?? false }
    var downloadsOn: Bool { core?.sharing()?.downloads ?? false }

    func refresh() {
        guard let core else { return }
        Task {
            do {
                links = try await core.shares()
                problem = nil
            } catch {
                problem = Self.words(error)
            }
        }
    }

    func create(_ target: ShareTargetRow, description: String, expires: UInt64?, downloadable: Bool) async throws -> ShareLinkRow {
        guard let core else { throw ShareError.noServer }
        do {
            let link = try await core.createShare(target: target, description: description, expiresMs: expires, downloadable: downloadable)
            refresh()
            return link
        } catch { throw ShareError.failed(Self.words(error)) }
    }

    func update(_ link: ShareLinkRow, description: String, expires: UInt64?, downloadable: Bool) async throws {
        guard let core else { throw ShareError.noServer }
        do {
            try await core.updateShare(link: link, description: description, expiresMs: expires, downloadable: downloadable)
            refresh()
        } catch { throw ShareError.failed(Self.words(error)) }
    }

    func delete(_ id: String) async -> String? {
        guard let core else { return L("Sharing is turned off on this server") }
        do {
            try await core.deleteShare(id: id)
            refresh()
            return nil
        } catch { return Self.words(error) }
    }

    private static func words(_ error: Error) -> String {
        if case .Failed(let message) = error as? NaviaError { return L(message) }
        return "\(error)"
    }

    /// When a link expires: the server's default or current date, a preset, or a chosen day.
    enum Expiry: Hashable { case first, days(Int), custom }

    static let presets: [(id: String, days: Int)] = [("1d", 1), ("1w", 7), ("1m", 30)]

    /// The end of `day` in local time, as Unix ms.
    static func endOfDay(_ day: Date) -> UInt64? {
        let parts = Calendar.current.dateComponents([.year, .month, .day], from: day)
        guard let y = parts.year, let m = parts.month, let d = parts.day else { return nil }
        return shareEndOfDayMs(year: Int32(y), month: UInt32(m), day: UInt32(d))
    }

    static func nowMs() -> UInt64 { UInt64(Date().timeIntervalSince1970 * 1000) }

    static func day(_ ms: UInt64) -> String { fmtDate(ISO8601DateFormatter().string(from: Date(timeIntervalSince1970: Double(ms) / 1000))) }

    /// The description, or empty when it's only the server's stand-in label.
    static func ownDescription(_ description: String, title: String) -> String {
        let d = description.trimmingCharacters(in: .whitespaces)
        let cut = d.hasSuffix("...") ? String(d.dropLast(3)) : d
        if d.isEmpty || title == d || (description.count >= 30 && title.hasPrefix(cut)) { return "" }
        return d
    }
}

enum ShareError: LocalizedError {
    case noServer, failed(String)
    var errorDescription: String? {
        switch self {
        case .noServer: L("Sharing is turned off on this server")
        case .failed(let words): words
        }
    }
}

/// What a link holds, for its dialogs and row: a title, its kind with the artist or album, and its cover.
struct ShareSubject {
    let title: String
    let kind: String
    let detail: String
    var art: URL? = nil
    var round = false
    var playlist: PlaylistRow? = nil
}
