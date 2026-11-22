import AppKit
import SwiftUI

enum PageNav: Equatable {
    case icons, none
    case search(String)
    case tabs(DiscoverTab)
}

/// The frame every page sits in, with the top bar floating over the content.
struct PageFrame<Right: View, Content: View>: View {
    let shell: ShellModel
    var nav: PageNav = .icons
    var title: String? = nil
    var detail: String? = nil
    /// for the AppKit table, which SwiftUI's scroll edge effect can't reach.
    var edgeFade = false
    /// Whether the right side has anything in it, when the page decides that by state.
    var hasRight: Bool? = nil
    var refresh: PageRefresh? = nil
    @ViewBuilder var right: Right
    @ViewBuilder var content: Content

    var body: some View {
        ZStack(alignment: .top) {
            content
            if edgeFade { TopEdgeFade() }
            TopBar(shell: shell, nav: nav, title: title, detail: detail, hasRight: hasRight, refresh: refresh) { right }
        }
    }
}

extension PageFrame where Right == EmptyView {
    init(shell: ShellModel, nav: PageNav = .icons, title: String? = nil, detail: String? = nil, edgeFade: Bool = false, refresh: PageRefresh? = nil, @ViewBuilder content: () -> Content) {
        self.init(shell: shell, nav: nav, title: title, detail: detail, edgeFade: edgeFade, refresh: refresh, right: { EmptyView() }, content: content)
    }
}

struct TopBar<Right: View>: View {
    let shell: ShellModel
    let nav: PageNav
    var title: String?
    var detail: String?
    var hasRight: Bool? = nil
    var refresh: PageRefresh? = nil
    @ViewBuilder var right: Right
    @Environment(\.pageContext) private var ctx

    private var css: CGFloat { ctx.cssWidth }
    private var crowded: Bool {
        if case .search = nav { return (hasRight ?? (Right.self != EmptyView.self)) && css <= 1040 }
        return false
    }

    /// with something in the middle, both sides take half so it stays centred; without, the right side's controls come first
    /// and the left (and its title) takes what's left.
    private var centred: Bool { nav != .none }

    var body: some View {
        HStack(spacing: css > 620 ? T.px(12) : T.px(8)) {
            left.frame(maxWidth: crowded ? nil : .infinity, alignment: .leading)
            center
            HStack(spacing: T.px(10)) { if !crowded && centred { Spacer(minLength: 0) }; right; if let refresh { RefreshButton(library: shell.library, kind: refresh) }; AccountButton(shell: shell) }
                .frame(maxWidth: crowded || !centred ? nil : .infinity, alignment: .trailing)
                .layoutPriority(centred ? 0 : 1)
        }
        .padding(.horizontal, T.gutter)
        .frame(height: T.topbarH)
        .padding(.leading, ctx.sidebarUnder)
        .padding(.trailing, ctx.dockUnder)
    }

    private var left: some View {
        HStack(spacing: T.px(10)) {
            if !ctx.sidebarOpen {
                IconButton(symbol: "sidebar.left", label: L("Show sidebar"), action: ctx.toggleSidebar)
                    .foregroundStyle(T.t2.color)
                    .padding(.leading, T.windowControlsW)
                    .padding(.trailing, T.px(8))
            }
            GlassGroup {
                IconButton(symbol: "chevron.left", label: L("Back"), weight: .semibold, disabled: !shell.canGoBack) { shell.goBack() }
                GlassDivider()
                IconButton(symbol: "chevron.right", label: L("Forward"), weight: .semibold, disabled: !shell.canGoForward) { shell.goForward() }
            }
            if let title, css > 620 {
                // whole or not at all: a cut title says nothing.
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: T.px(10)) {
                        Text(title).font(.navia(T.textMD, .semibold)).foregroundStyle(T.tEm.color)
                        if let detail, css > 900 {
                            Text(detail).font(.navia(T.textMD)).foregroundStyle(T.t2.color)
                        }
                    }
                    .lineLimit(1)
                    .fixedSize()
                    Color.clear.frame(width: 0, height: 0)
                }
            }
            if !crowded { Spacer(minLength: 0) }
        }
    }

    @ViewBuilder private var center: some View {
        switch nav {
        case .none: EmptyView()
        case .icons: CenterPill(shell: shell, tabWidth: T.px(css > 900 ? 75 : css > 620 ? 54 : 42))
        case .search(let query): SearchField(shell: shell, query: query, crowded: crowded)
        case .tabs(let active): TabPill(shell: shell, active: active, padding: T.px(css > 900 ? 24 : css > 620 ? 14 : 10))
        }
    }
}

struct CenterPill: View {
    let shell: ShellModel
    let tabWidth: CGFloat

    private struct Item {
        let route: Route
        let symbol: String
        let label: String
    }

    private let items = [
        Item(route: .home, symbol: "house", label: L("Home")),
        Item(route: .discover(.new), symbol: "flame", label: L("New")),
        Item(route: .discover(.forYou), symbol: "sparkle", label: L("Discover")),
        Item(route: .albums(.all), symbol: "music.note.square.stack", label: L("Library")),
        Item(route: .browse(), symbol: "magnifyingglass", label: L("Search")),
    ]

    var body: some View {
        HStack(spacing: 0) {
            ForEach(items, id: \.label) { item in
                PillTab(symbol: item.symbol, label: item.label, on: isOn(item), width: tabWidth) { shell.go(item.route) }
            }
        }
        .padding(T.px(4))
        .frame(height: T.pillH)
        .glassPill()
        .shadow(T.shadowPill)
    }

    private func isOn(_ item: Item) -> Bool {
        switch shell.route {
        case .home: item.label == L("Home")
        case .discover(.new): item.label == L("New")
        case .discover: item.label == L("Discover")
        case .albums, .album, .songs, .liked, .artists, .artist, .artistTop, .playlists, .playlist: item.label == L("Library")
        case .browse, .search: item.label == L("Search")
        case .settings, .equalizer, .downloads, .cachedSongs, .shares, .smartEditor, .mix, .upcoming: false
        }
    }
}

private struct PillTab: View {
    let symbol: String
    let label: String
    let on: Bool
    let width: CGFloat
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: T.px(20), weight: .regular))
                .symbolVariant(on ? .fill : .none)
                .foregroundStyle((on ? T.tEm : hovering ? T.t1 : T.t2).color)
                .frame(width: width, height: T.px(48))
                .background(Capsule().fill(on ? T.glassOn.color : .clear))
                .animation(T.toggle, value: on)
                .contentShape(Capsule())
        }
        .buttonStyle(HoverBrightenStyle(selected: on, key: label))
        .onHover { hovering = $0 }
        .help(label)
        .accessibilityLabel(label)
    }
}

private struct TabPill: View {
    let shell: ShellModel
    let active: DiscoverTab
    let padding: CGFloat

    var body: some View {
        HStack(spacing: 0) {
            ForEach(DiscoverTab.allCases) { tab in
                DiscoverTabButton(title: tab.label, on: tab == active, padding: padding) { shell.go(.discover(tab)) }
            }
        }
        .padding(T.px(4))
        .fixedSize(horizontal: true, vertical: false)
        .frame(height: T.px(53))
        .glassPill()
        .shadow(T.shadowPill)
    }
}

private struct DiscoverTabButton: View {
    let title: String
    let on: Bool
    let padding: CGFloat
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Text(title)
                .font(.navia(T.textMD, .medium))
                .foregroundStyle((on ? T.tEm : hovering ? T.t1 : T.t2).color)
                .lineLimit(1)
                .padding(.horizontal, padding)
                .frame(height: T.px(44))
                .background(Capsule().fill(on ? T.glassOn.color : .clear))
                .animation(T.toggle, value: on)
                .contentShape(Capsule())
        }
        .buttonStyle(HoverBrightenStyle(selected: on, key: title))
        .onHover { hovering = $0 }
    }
}

/// The search field in the top pill on Search and Browse. The first letter typed on Browse adds a history step,
/// and later letters replace it.
struct SearchField: View {
    let shell: ShellModel
    let query: String
    let crowded: Bool
    @FocusState private var focused: Bool
    @State private var text = ""
    @Environment(\.pageContext) private var ctx
    @Environment(\.isSnapshot) private var isSnapshot

    private var width: CGFloat {
        if crowded { return .infinity }
        let css = ctx.cssWidth
        let base: CGFloat = css <= 1000 ? 260 : 384
        return T.px(min(base, css * 0.42 + 60))
    }

    var body: some View {
        HStack(spacing: T.px(11)) {
            Image(systemName: "magnifyingglass").font(.system(size: T.px(19)))
            if isSnapshot {
                Text(query.isEmpty ? L("Search...") : query).font(.navia(T.textLG)).foregroundStyle((query.isEmpty ? T.t2 : T.t1).color)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                TextField("", text: $text, prompt: Text(L("Search...")).foregroundStyle(T.t2.color))
                    .textFieldStyle(.plain)
                    .font(.navia(T.textLG))
                    .focused($focused)
                    .onChange(of: text) { _, value in typed(value) }
                    .onChange(of: query) { _, value in if value != text { text = value } }
                    .escapeClears($text)
                    .holdsWindowStill()
                SearchClearButton(text: $text, size: T.searchClearLG)
            }
        }
        .padding(.horizontal, T.px(19))
        .frame(height: T.px(53))
        .frame(maxWidth: crowded ? .infinity : width)
        .frame(minWidth: crowded ? T.px(120) : nil)
        .glassPill()
        .focusRing(focused, shell.theme.accent)
        .shadow(T.shadowPill)
        .onAppear { text = query }
        // focus waits a turn: on first appearance the field isn't in the window yet.
        .task(id: shell.searchFocus) { await Task.yield(); focused = true }
    }

    private func typed(_ value: String) {
        if value.isEmpty {
            shell.replace(.browse())
        } else if case .search = shell.route {
            shell.replace(.search(value))
        } else {
            shell.go(.search(value))
        }
    }
}

struct SearchPill: View {
    let placeholder: String
    @Binding var text: String
    let accent: RGBA
    let width: CGFloat
    @FocusState private var focused: Bool
    @Environment(\.isSnapshot) private var isSnapshot

    private static var glyph: CGFloat { T.px(16) }

    var body: some View {
        HStack(spacing: T.searchPillGap) {
            Image(systemName: "magnifyingglass").font(.system(size: Self.glyph)).foregroundStyle(T.t2.color)
            if isSnapshot {
                Text(placeholder).font(.navia(T.textMD)).foregroundStyle(T.t3.color).lineLimit(1).frame(maxWidth: .infinity, alignment: .leading)
            } else {
                TextField("", text: $text)
                    .textFieldStyle(.plain)
                    .font(.navia(T.textMD))
                    .focused($focused)
                    .escapeClears($text)
                    .holdsWindowStill()
                    .accessibilityLabel(placeholder)
                    .overlay(alignment: .leading) {
                        if text.isEmpty {
                            // where the bar leaves no room for the page's own words whole, plain "Search" instead of a cut one.
                            ViewThatFits(in: .horizontal) { Text(placeholder); Text(L("Search")) }
                                .font(.navia(T.textMD))
                                .foregroundStyle(T.t3.color)
                                .lineLimit(1)
                                .padding(.leading, T.fieldTextInset)
                                .allowsHitTesting(false)
                                .accessibilityHidden(true)
                        }
                    }
                SearchClearButton(text: $text)
            }
        }
        .padding(.horizontal, T.searchPillPad)
        .frame(minWidth: width, idealWidth: fit, maxWidth: fit)
        .frame(height: T.controlH)
        .glassPill()
        .focusRing(focused, accent)
        // ahead of the spacer beside it, so it only narrows when the bar is full.
        .layoutPriority(1)
    }

    private var fit: CGFloat { max(width, Self.fitting(placeholder)) }

    /// wide enough to show `placeholder` whole, in any language.
    private static func fitting(_ placeholder: String) -> CGFloat {
        let text = (placeholder as NSString).size(withAttributes: [.font: NSFont.systemFont(ofSize: T.textMD)]).width
        let mark = NSImage(systemSymbolName: "magnifyingglass", accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: glyph, weight: .regular))?.size.width ?? glyph
        return (text + mark + T.searchPillGap + 2 * (T.searchPillPad + T.fieldTextInset)).rounded(.up)
    }
}

/// The search field's width, in steps by page width.
func pillSearchWidth(_ ctx: PageContext) -> CGFloat { T.px(ctx.cssWidth > 1000 ? 187 : ctx.cssWidth > 620 ? 168 : 120) }

struct SortButton<Option: Identifiable & Equatable>: View {
    let symbol: String
    let label: String
    let options: [Option]
    let title: (Option) -> String
    let current: Option
    let choose: (Option) -> Void

    var body: some View {
        MenuIconButton(symbol: symbol, label: label, size: T.px(16)) {
            options.map { option in
                MenuEntry(title: title(option), checked: option == current) { choose(option) }
            }
        }
    }
}
