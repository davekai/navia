import Foundation
import NaviaCoreFFI
import Observation

/// Which page's filters, Songs or Artists.
enum FilterPage { case songs, artists }

/// A page's filters. The core owns the catalog, the choices and the matching. This keeps what's set, in order,
/// and hands it to the page's list.
@Observable final class FiltersModel {
    @ObservationIgnored let page: FilterPage
    /// Every filter in chip order. Labels are English catalog keys.
    @ObservationIgnored let catalog: [SongFilterInfo]
    @ObservationIgnored private let core: NaviaCore?
    /// The filters this library has data for, in chip order. Empty until read, and in sample mode.
    private(set) var available: [SongFilterId] = []
    private(set) var settings: [SongFilterSetting] = []
    /// Filters added from More Filters stay as chips for the visit, even once cleared.
    var added: [SongFilterId] = []
    /// A filter just added from More Filters, whose chip opens its popover once it's in place.
    var pendingOpen: SongFilterId?
    @ObservationIgnored private var optionsCache: [SongFilterId: [SongFilterOptionRow]] = [:]
    @ObservationIgnored private var quickCache: [SongFilterId: [SongFilterQuickRow]] = [:]
    @ObservationIgnored var onChange: (([SongFilterSetting]) -> Void)?

    init(core: NaviaCore?, page: FilterPage, genre: String? = nil) {
        self.core = core
        self.page = page
        catalog = page == .songs ? songFilterCatalog() : artistFilterCatalog()
        #if DEBUG
        if page == .songs { FiltersDebug.songs = self } else { FiltersDebug.artists = self }
        #endif
        if let genre { settings = [SongFilterSetting(id: .genre, value: .list(values: [genre]))] }
    }

    /// Reads which filters the library can offer, off the main thread.
    func load() async {
        guard let core else { return }
        let songs = page == .songs
        let found = await Task.detached(priority: .userInitiated) { songs ? core.songFiltersAvailable() : core.artistFiltersAvailable() }.value
        optionsCache = [:]
        quickCache = [:]
        available = found
    }

    func info(_ id: SongFilterId) -> SongFilterInfo { catalog.first { $0.id == id }! }
    func value(_ id: SongFilterId) -> SongFilterValue? { settings.first { $0.id == id }?.value }
    var isFiltering: Bool { !settings.isEmpty }

    /// A typed range that isn't finished can't be saved.
    var unfinished: Bool {
        settings.contains { s in
            if case .range(let from, let to) = s.value { return songFilterRangeProblem(id: s.id, from: from, to: to) != nil }
            return false
        }
    }

    /// the main set, those added from More Filters, and any that are set.
    var shown: [SongFilterId] { available.filter { info($0).main || added.contains($0) || value($0) != nil } }
    var rest: [SongFilterId] { available.filter { !shown.contains($0) } }

    /// Sets or clears one filter. An empty list or range clears it.
    func set(_ id: SongFilterId, _ value: SongFilterValue?) {
        var next = settings
        let empty: Bool
        switch value {
        case .none: empty = true
        case .list(let values): empty = values.isEmpty
        case .range(let from, let to): empty = from.isEmpty && to.isEmpty
        case .on: empty = false
        }
        if empty {
            next.removeAll { $0.id == id }
        } else if let value, let at = next.firstIndex(where: { $0.id == id }) {
            next[at] = SongFilterSetting(id: id, value: value)
        } else if let value {
            next.append(SongFilterSetting(id: id, value: value))
        }
        settings = next
        onChange?(next)
    }

    func clear() {
        settings = []
        onChange?([])
    }

    /// Toggles a choice of a list filter. A single-choice list keeps at most one.
    func toggleChoice(_ id: SongFilterId, _ choice: String) {
        var list: [String] = []
        if case .list(let values) = value(id) { list = values }
        if let at = list.firstIndex(of: choice) { list.remove(at: at) } else if info(id).single { list = [choice] } else { list.append(choice) }
        set(id, .list(values: list))
    }

    func options(_ id: SongFilterId) -> [SongFilterOptionRow] {
        if let cached = optionsCache[id] { return cached }
        let found = (page == .songs ? core?.songFilterOptions(id: id) : core?.artistFilterOptions(id: id)) ?? []
        optionsCache[id] = found
        return found
    }

    func quick(_ id: SongFilterId) -> [SongFilterQuickRow] {
        if let cached = quickCache[id] { return cached }
        let found = page == .songs ? core?.songFilterQuick(id: id) ?? [] : []
        quickCache[id] = found
        return found
    }

    func label(_ option: SongFilterOptionRow) -> String { option.translate ? L(option.label) : option.label }

    /// The text after an active chip's name, like "Rock +2" or "1990 – 1999".
    func summary(_ id: SongFilterId, _ value: SongFilterValue) -> String {
        switch value {
        case .on: return L(info(id).label)
        case .range(let from, let to):
            if !from.isEmpty && !to.isEmpty { return from == to ? from : "\(from) – \(to)" }
            return from.isEmpty ? L("Up to {value}", ["value": to]) : L("From {value}", ["value": from])
        case .list(let values):
            let first = values.first ?? ""
            let name = options(id).first { $0.value == first }.map(label) ?? first
            return values.count > 1 ? "\(name) +\(values.count - 1)" : name
        }
    }

    /// Navidrome's smart playlist rules for the set filters and sort, and the names of filters the server has no
    /// field for.
    func rules(order: SongOrderBy) -> (json: String, dropped: String?) {
        let found = songFilterRules(filters: settings, order: order)
        let dropped = found.dropped.map { L(info($0).label) }
        return (found.json, dropped.isEmpty ? nil : dropped.joined(separator: ", "))
    }
}

#if DEBUG
/// CoreCheck's `filter=PAGE:ID:VALUE` and `filterClear=PAGE`. PAGE is songs or artists, and VALUE is `on`,
/// `clear`, `open`, or choices joined by `|`.
enum FiltersDebug {
    nonisolated(unsafe) static weak var songs: FiltersModel?
    nonisolated(unsafe) static weak var artists: FiltersModel?

    @MainActor static func apply(_ spec: String) {
        let parts = spec.split(separator: ":", maxSplits: 2).map(String.init)
        guard parts.count == 3, let model = parts[0] == "songs" ? songs : artists,
              let id = model.catalog.first(where: { "\($0.id)" == parts[1] })?.id else { return }
        switch parts[2] {
        case "on": model.set(id, .on)
        case "clear": model.set(id, nil)
        case "open": model.pendingOpen = id
        default: model.set(id, .list(values: parts[2].split(separator: "|").map(String.init)))
        }
    }

    @MainActor static func clear(_ page: String) { (page == "songs" ? songs : artists)?.clear() }
}
#endif
