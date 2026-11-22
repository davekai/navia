import Foundation

/// Translations. The catalog lives in `locales/*.json` at the repo root and is shared with GPUI.
/// `tools/i18n.py sync` copies it into `Resources/Locales`. Keys are the English text.
///
///     L("Settings")
///     L("Added to {name}", ["name": value])
///     Ln("{count} song", "{count} songs", n)
///     N_("Album Artist")   // marks a key in a table; show it with L(value)
///
/// A missing key falls back to English, and a raw key is never shown.
nonisolated enum Language {
    static let codes = ["en", "fr", "es", "de"]
    /// each language's name in that language; never translated.
    static let names: [(code: String, name: String)] = [("en", "English"), ("fr", "Français"), ("es", "Español"), ("de", "Deutsch")]
}

nonisolated private enum CatalogText {
    case plain(String)
    case plural(one: String, many: String?, other: String)
}

nonisolated private final class Catalogs: @unchecked Sendable {
    static let shared = Catalogs()

    private let lock = NSLock()
    private var tables: [String: [String: CatalogText]] = [:]
    private var active = "en"
    private var formatter = NumberFormatter()

    private init() {
        for code in Language.codes { tables[code] = Self.load(code) }
        setActive("en")
    }

    private static func load(_ code: String) -> [String: CatalogText] {
        guard let url = Bundle.main.url(forResource: code, withExtension: "json", subdirectory: "Locales") ?? Bundle.main.url(forResource: code, withExtension: "json"),
              let data = try? Data(contentsOf: url),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return [:]
        }
        var out: [String: CatalogText] = [:]
        out.reserveCapacity(json.count)
        for (key, value) in json {
            if let text = value as? String {
                out[key] = .plain(text)
            } else if let forms = value as? [String: String], let other = forms["other"] {
                out[key] = .plural(one: forms["one"] ?? other, many: forms["many"], other: other)
            }
        }
        return out
    }

    func setActive(_ code: String) {
        lock.lock(); defer { lock.unlock() }
        active = code
        let f = NumberFormatter()
        f.locale = Locale(identifier: code)
        f.numberStyle = .decimal
        f.usesGroupingSeparator = true
        formatter = f
    }

    private func entry(_ key: String) -> (CatalogText, String)? {
        lock.lock(); defer { lock.unlock() }
        if let text = tables[active]?[key] { return (text, active) }
        if let text = tables["en"]?[key] { return (text, "en") }
        return nil
    }

    func plain(_ key: String) -> String {
        if case .plain(let text)? = entry(key)?.0 { return text }
        return String(key.split(separator: "||", maxSplits: 1, omittingEmptySubsequences: false).first ?? Substring(key))
    }

    func plural(one: String, other: String, n: Int) -> String {
        guard let (text, language) = entry(other), case .plural(let oneForm, let many, let otherForm) = text else {
            return n == 1 ? one : other
        }
        switch category(language, n) {
        case "one": return oneForm
        case "many": return many ?? otherForm
        default: return otherForm
        }
    }

    private func category(_ language: String, _ n: Int) -> String {
        let millions = n != 0 && n % 1_000_000 == 0
        switch language {
        case "fr": return n < 2 ? "one" : (millions ? "many" : "other")
        case "es": return n == 1 ? "one" : (millions ? "many" : "other")
        default: return n == 1 ? "one" : "other"
        }
    }

    func number(_ n: Int) -> String {
        lock.lock(); defer { lock.unlock() }
        return Self.withNBSP(formatter.string(from: NSNumber(value: n)) ?? String(n))
    }

    func decimal(_ value: Double, digits: Int) -> String {
        lock.lock(); defer { lock.unlock() }
        formatter.minimumFractionDigits = digits
        formatter.maximumFractionDigits = digits
        defer { formatter.minimumFractionDigits = 0; formatter.maximumFractionDigits = 0 }
        return Self.withNBSP(formatter.string(from: NSNumber(value: value)) ?? String(value))
    }

    /// A date with a short month, like "Mar 4, 2025". Empty for a missing or bad date.
    func date(_ iso: String?) -> String {
        guard let iso, !iso.isEmpty else { return "" }
        let parser = ISO8601DateFormatter()
        parser.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        var day = parser.date(from: iso)
        if day == nil { parser.formatOptions = [.withInternetDateTime]; day = parser.date(from: iso) }
        guard let day else { return "" }
        lock.lock(); let code = active; lock.unlock()
        return Self.withNBSP(day.formatted(Date.FormatStyle(date: .abbreviated, time: .omitted).locale(Locale(identifier: code))))
    }

    /// A date with its local time, like "Apr 7, 2026 at 23:05" in the language's own form.
    func dateTime(_ iso: String?) -> String {
        guard let iso, !iso.isEmpty else { return "" }
        let parser = ISO8601DateFormatter()
        parser.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        var at = parser.date(from: iso)
        if at == nil { parser.formatOptions = [.withInternetDateTime]; at = parser.date(from: iso) }
        guard let at else { return "" }
        lock.lock(); let code = active; lock.unlock()
        return Self.withNBSP(at.formatted(Date.FormatStyle(date: .abbreviated, time: .shortened).locale(Locale(identifier: code))))
    }

    /// A release date as precise as the server has it, with the full month.
    func release(year: Int, month: Int?, day: Int?) -> String {
        guard let month, (1...12).contains(month) else { return String(year) }
        var parts = DateComponents()
        (parts.year, parts.month, parts.day) = (year, month, day ?? 1)
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        guard let date = calendar.date(from: parts) else { return String(year) }
        lock.lock(); let code = active; lock.unlock()
        var style = Date.FormatStyle(timeZone: calendar.timeZone).locale(Locale(identifier: code)).year().month(.wide)
        if day != nil { style = style.day() }
        return Self.withNBSP(date.formatted(style))
    }

    private static func withNBSP(_ s: String) -> String { s.replacingOccurrences(of: "\u{202F}", with: "\u{00A0}") }

    var language: String { lock.lock(); defer { lock.unlock() }; return active }
}

/// Sets the language shown. `"system"` or nil follows the system language.
nonisolated func setLanguage(_ choice: String?) {
    Catalogs.shared.setActive(resolveLanguage(choice))
}

nonisolated func currentLanguage() -> String { Catalogs.shared.language }

nonisolated func resolveLanguage(_ choice: String?) -> String {
    if let choice, choice != "system", Language.codes.contains(choice) { return choice }
    for preferred in Locale.preferredLanguages {
        let code = preferred.split(whereSeparator: { $0 == "-" || $0 == "_" }).first.map(String.init)?.lowercased() ?? ""
        if Language.codes.contains(code) { return code }
    }
    return "en"
}

nonisolated func L(_ key: String) -> String { Catalogs.shared.plain(key) }

/// Marks `key` as translatable in a table. Show it with `L(value)`.
nonisolated func N_(_ key: String) -> String { key }

/// values are inserted as they are.
nonisolated func L(_ key: String, _ vars: [String: String]) -> String {
    fillPlaceholders(Catalogs.shared.plain(key), vars)
}

/// Counted text. `one` and `other` are the English forms, and the catalog key is `other`.
nonisolated func Ln(_ one: String, _ other: String, _ n: Int, _ vars: [String: String] = [:]) -> String {
    var all = vars
    all["count"] = Catalogs.shared.number(n)
    return fillPlaceholders(Catalogs.shared.plural(one: one, other: other, n: n), all)
}

/// like 12,345 or 12.345.
nonisolated func fmtInt(_ n: Int) -> String { Catalogs.shared.number(n) }

/// The language's form of a counted text for `n`, with `{count}` left in for the core to fill.
nonisolated func pluralTemplate(_ one: String, _ other: String, _ n: Int) -> String { Catalogs.shared.plural(one: one, other: other, n: n) }

/// A date from an ISO 8601 time with a short month, like Mar 4, 2025.
nonisolated func fmtDate(_ iso: String?) -> String { Catalogs.shared.date(iso) }

/// like 12.5 or 12,5.
nonisolated func fmtDec(_ value: Double, _ digits: Int) -> String { Catalogs.shared.decimal(value, digits: digits) }
/// A server time with its local time, for Get Info's Created and Changed.
nonisolated func fmtDateTime(_ iso: String?) -> String { Catalogs.shared.dateTime(iso) }
/// "2011", "March 2011" or "March 4, 2011" in the language's form.
nonisolated func fmtRelease(year: Int, month: Int?, day: Int?) -> String { Catalogs.shared.release(year: year, month: month, day: day) }

nonisolated private func fillPlaceholders(_ template: String, _ vars: [String: String]) -> String {
    guard template.contains("{") else { return template }
    var out = ""
    out.reserveCapacity(template.count + 16)
    var rest = Substring(template)
    while let open = rest.firstIndex(of: "{"), let close = rest[open...].firstIndex(of: "}") {
        out += rest[..<open]
        let name = String(rest[rest.index(after: open)..<close])
        if let value = vars[name] { out += value } else { out += rest[open...close] }
        rest = rest[rest.index(after: close)...]
    }
    out += rest
    return out
}
