import AppKit
import SwiftUI

struct MenuEntry {
    var title = ""
    var checked = false
    var enabled = true
    /// shown as the disabled row's tooltip.
    var reason: String? = nil
    var isDivider = false
    /// the row's action isn't called.
    var submenu: [MenuEntry]? = nil
    /// With a submenu, the placeholder of a filter field at its top. Typing narrows the submenu's `filterable` rows.
    var find: String? = nil
    /// shown in place of the rows when the filter matches nothing.
    var empty: String? = nil
    /// the playlists, not New Playlist.
    var filterable = false
    /// An SF Symbol for the row. Without one, `MenuIcons` looks the title up, so an action has the same icon in every
    /// menu.
    var symbol: String? = nil
    /// A key equivalent shown at the row's end, such as "," for Settings. Command unless `shortcutModifiers` says otherwise.
    var shortcut: String? = nil
    var shortcutModifiers: NSEvent.ModifierFlags = .command
    /// flips without closing the menu (needs `checked`).
    var isSwitch = false
    var view: NSView? = nil
    /// A section heading, drawn as the system's section header.
    var isHeader = false
    /// A second, dimmer line under the title.
    var subtitle: String? = nil
    /// an icon button in the row above the list; the title is its tooltip and accessible name, `checked` fills the icon.
    var quick = false
    var action: () -> Void = {}

    static let divider = MenuEntry(isDivider: true)
}

/// Menu icons as SF Symbols, keyed by the row's English text. Titles are matched after translation.
enum MenuIcons {
    @MainActor static func symbol(for title: String) -> String? { table[title] }

    @MainActor private static var table: [String: String] {
        if let cached, cachedLanguage == currentLanguage() { return cached }
        let pairs: [(String, String)] = [
            ("Play Next", "arrow.turn.up.right"), ("Add to Queue", "text.line.first.and.arrowtriangle.forward"),
            ("Add to Playlist", "text.badge.plus"), ("Remove Download", "xmark"), ("Cancel Download", "xmark"),
            ("Remove Downloads", "xmark"), ("Download Selected", "arrow.down"),
            ("Go to Album", "opticaldisc"), ("Go to Artist", "music.microphone"), ("Song Radio", "radio"),
            ("Artist Radio", "radio"), ("Share…", "square.and.arrow.up"), ("Stop Station", "stop"),
            ("Private Mode", "eye.slash"), ("Offline", "icloud.slash"), ("Mini Player", "pip"), ("Settings…", "gearshape"), ("Change Server", "server.rack"),
            ("Sign Out", "rectangle.portrait.and.arrow.right"), ("Public on this server", "person.2"),
            ("Create Share Link", "link"), ("Edit Details…", "pencil"), ("Edit Rules", "bolt"),
            ("Album Collage", "photo"), ("Generated Cover", "sparkle"), ("Upload Image", "square.and.arrow.up.on.square"),
            ("Remove Image", "trash"), ("Export as M3U", "square.and.arrow.up.on.square"),
            ("New Playlist", "music.note.list"), ("New Smart Playlist", "bolt"), ("Remove from Playlist", "minus.circle"),
        ]
        let built = Dictionary(pairs.map { (L($0.0), $0.1) }, uniquingKeysWith: { a, _ in a })
        cached = built
        cachedLanguage = currentLanguage()
        return built
    }

    @MainActor private static var cached: [String: String]?
    @MainActor private static var cachedLanguage = ""

    /// A template image in a title attachment isn't tinted by the menu, so the color is applied to the symbol itself.
    /// `nil` gives an empty cell, for rows without an icon in a menu that has some.
    @MainActor static func image(_ symbol: String?, dimmed: Bool = false) -> NSImage {
        let color: NSColor = dimmed ? .disabledControlTextColor : .labelColor
        let config = NSImage.SymbolConfiguration(pointSize: T.menuIcon, weight: .regular).applying(NSImage.SymbolConfiguration(paletteColors: [color]))
        let glyph = symbol.flatMap { NSImage(systemSymbolName: $0, accessibilityDescription: nil)?.withSymbolConfiguration(config) }
        let image = NSImage(size: T.menuIconCell, flipped: false) { rect in
            guard let glyph else { return true }
            let scale = min(1, T.menuIconFitW / glyph.size.width)
            let s = NSSize(width: glyph.size.width * scale, height: glyph.size.height * scale)
            glyph.draw(in: NSRect(x: rect.midX - s.width / 2, y: rect.midY - s.height / 2, width: s.width, height: s.height))
            return true
        }
        image.isTemplate = false
        return image
    }

    /// The row's title led by its icon. macOS 27 doesn't draw `NSMenuItem.image` in these menus, but an image in the
    /// attributed title draws and takes the row's text color.
    @MainActor static func title(_ text: String, symbol: String?, enabled: Bool) -> NSAttributedString {
        let font = NSFont.menuFont(ofSize: 0)
        let attachment = NSTextAttachment()
        attachment.image = image(symbol, dimmed: !enabled)
        let cell = T.menuIconCell
        attachment.bounds = NSRect(x: 0, y: (font.capHeight - cell.height) / 2, width: cell.width, height: cell.height)
        let out = NSMutableAttributedString(attachment: attachment)
        var attributes: [NSAttributedString.Key: Any] = [.font: font]
        if !enabled { attributes[.foregroundColor] = NSColor.disabledControlTextColor }
        out.addAttributes(attributes, range: NSRange(location: 0, length: out.length))
        out.append(NSAttributedString(string: "  " + text, attributes: attributes))
        return out
    }
}

private final class MenuHandler: NSObject {
    let run: () -> Void
    init(_ run: @escaping () -> Void) { self.run = run }
    @objc func fire() { run() }
}

/// A right-click menu for one thing outside a list, as `.rightClickMenu { entries }`. Only right-clicks and
/// Control-clicks reach it. Other clicks pass through.
struct RightClickMenu: NSViewRepresentable {
    let entries: () -> [MenuEntry]

    func makeNSView(context: Context) -> RightClickMenuView { RightClickMenuView() }
    func updateNSView(_ view: RightClickMenuView, context: Context) { view.entries = entries }
}

final class RightClickMenuView: NSView {
    var entries: () -> [MenuEntry] = { [] }

    override func hitTest(_ point: NSPoint) -> NSView? {
        guard let event = NSApp.currentEvent else { return nil }
        let menuClick = event.type == .rightMouseDown || (event.type == .leftMouseDown && event.modifierFlags.contains(.control))
        return menuClick ? super.hitTest(point) : nil
    }

    override func menu(for event: NSEvent) -> NSMenu? {
        let items = entries()
        return items.isEmpty ? nil : makeMenu(items)
    }
}

extension View {
    func rightClickMenu(_ entries: @escaping () -> [MenuEntry]) -> some View { overlay(RightClickMenu(entries: entries)) }
}

func makeMenu(_ entries: [MenuEntry], find: String? = nil, empty: String? = nil) -> NSMenu {
    let menu = NSMenu()
    menu.autoenablesItems = false
    fill(menu, with: entries, find: find, empty: empty)
    return menu
}

/// replaces the menu's rows; works while the menu is open.
private func fill(_ menu: NSMenu, with entries: [MenuEntry], find: String? = nil, empty: String? = nil) {
    menu.removeAllItems()
    var filter: MenuFindField?
    if let find {
        let field = MenuFindField(placeholder: find)
        let item = NSMenuItem()
        item.view = field
        menu.addItem(item)
        menu.delegate = field
        filter = field
    }
    let quick = entries.filter(\.quick)
    var entries = entries.filter { !$0.quick }
    var quickRow: MenuQuickRow?
    if !quick.isEmpty {
        let item = NSMenuItem()
        let row = MenuQuickRow(quick)
        quickRow = row
        item.view = row
        menu.addItem(item)
        // the list under the row starts without its own divider: the row's rule separates them.
        while entries.first?.isDivider == true { entries.removeFirst() }
        if !entries.isEmpty { menu.addItem(.separator()) }
    }
    let symbols = entries.map { $0.isDivider || $0.isHeader || $0.view != nil ? nil : ($0.symbol ?? MenuIcons.symbol(for: $0.title)) }
    let anyIcon = symbols.contains { $0 != nil }
    for (index, entry) in entries.enumerated() {
        if entry.isDivider { menu.addItem(.separator()); continue }
        if entry.isHeader { menu.addItem(.sectionHeader(title: entry.title)); continue }
        if let view = entry.view {
            let item = NSMenuItem()
            item.view = view
            menu.addItem(item)
            continue
        }
        let item = NSMenuItem(title: entry.title, action: #selector(MenuHandler.fire), keyEquivalent: entry.shortcut ?? "")
        if entry.shortcut != nil { item.keyEquivalentModifierMask = entry.shortcutModifiers }
        if anyIcon { item.attributedTitle = MenuIcons.title(entry.title, symbol: symbols[index], enabled: entry.enabled) }
        if entry.isSwitch {
            item.view = MenuSwitchRow(title: entry.title, image: MenuIcons.image(symbols[index]), on: entry.checked, action: entry.action)
            menu.addItem(item)
            continue
        }
        if let submenu = entry.submenu {
            item.action = nil
            item.submenu = makeMenu(submenu, find: entry.find, empty: entry.empty)
        } else {
            let target = MenuHandler(entry.action)
            item.target = target
            item.representedObject = target // NSMenuItem holds its target weakly.
        }
        item.isEnabled = entry.enabled
        if !entry.enabled, let reason = entry.reason { item.toolTip = reason }
        if let subtitle = entry.subtitle { item.subtitle = subtitle }
        item.state = entry.checked ? .on : .off
        if entry.filterable { filter?.filterable.append(item) }
        menu.addItem(item)
    }
    if let filter, let empty {
        let item = NSMenuItem(title: empty, action: nil, keyEquivalent: "")
        item.isEnabled = false
        item.isHidden = true
        filter.emptyItem = item
        menu.addItem(item)
    }
    quickRow?.fit(menuWidth: menu.size.width)
}

/// keeps an open menu's rows in step with the observable state its entries read.
@MainActor private final class LiveMenu {
    private let menu: NSMenu
    private let entries: () -> [MenuEntry]
    private var shown: [String]
    private var open = true

    init(_ menu: NSMenu, entries: @escaping () -> [MenuEntry], shown: [MenuEntry]) {
        self.menu = menu
        self.entries = entries
        self.shown = shown.map(Self.key)
    }

    /// runs until `close`; the main queue it resumes on is still served while the menu tracks.
    func follow() {
        guard open else { return }
        let next = withObservationTracking(entries) {
            Task { @MainActor [weak self] in self?.follow() }
        }
        let keys = next.map(Self.key)
        guard keys != shown else { return }
        shown = keys
        fill(menu, with: next)
    }

    func close() { open = false }

    private static func key(_ e: MenuEntry) -> String {
        [e.title, e.subtitle ?? "", e.checked ? "1" : "0", e.enabled ? "1" : "0", e.isHeader ? "h" : e.isDivider ? "d" : ""].joined(separator: "\u{1f}")
    }
}

/// square icon buttons of one size in every menu, spread across the rows' width above the list.
final class MenuQuickRow: NSView {
    private let buttons: [MenuQuickButton]

    init(_ entries: [MenuEntry]) {
        buttons = entries.map(MenuQuickButton.init)
        super.init(frame: NSRect(x: 0, y: 0, width: Self.minWidth(count: entries.count), height: Self.height))
        autoresizingMask = [.width]
        buttons.forEach(addSubview)
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    private static func minWidth(count: Int) -> CGFloat {
        let n = CGFloat(count)
        return 2 * T.menuContentInset + T.menuQuickSize * (n + max(0, n - 1) * T.menuQuickGapRatio)
    }

    private static var height: CGFloat { T.menuQuickPadTop + T.menuQuickSize + T.menuQuickPadBottom }

    func fit(menuWidth: CGFloat) {
        setFrameSize(NSSize(width: menuWidth, height: Self.height))
    }

    override func setFrameSize(_ newSize: NSSize) { super.setFrameSize(newSize); place() }
    override func viewDidMoveToWindow() { place() }

    private func place() {
        guard buttons.count > 0 else { return }
        let size = T.menuQuickSize
        let free = bounds.width - 2 * T.menuContentInset - CGFloat(buttons.count) * size
        let gap = buttons.count > 1 ? max(0, free / CGFloat(buttons.count - 1)) : 0
        for (index, button) in buttons.enumerated() {
            button.frame = NSRect(x: T.menuContentInset + CGFloat(index) * (size + gap), y: T.menuQuickPadBottom, width: size, height: size)
        }
    }

    #if DEBUG
    /// for checks: the buttons' titles and frames, and runs one as a click would.
    var debugButtons: [(title: String, frame: NSRect)] { buttons.map { ($0.entry.title, $0.frame) } }
    func debugPress(_ index: Int) { buttons[index].press() }
    func debugHover(_ index: Int) { buttons[index].debugHover() }
    #endif
}

/// A click runs it and closes the menu; one with a submenu opens that list where the menu was.
final class MenuQuickButton: NSView {
    let entry: MenuEntry
    private let icon = NSImageView()
    private var hovering = false { didSet { needsDisplay = true; tint() } }

    init(_ entry: MenuEntry) {
        self.entry = entry
        super.init(frame: NSRect(x: 0, y: 0, width: T.menuQuickSize, height: T.menuQuickSize))
        icon.frame = bounds
        icon.autoresizingMask = [.width, .height]
        icon.imageScaling = .scaleNone
        addSubview(icon)
        tint()
        toolTip = !entry.enabled ? entry.reason ?? entry.title : entry.title
        setAccessibilityElement(true)
        setAccessibilityRole(.button)
        setAccessibilityLabel(entry.title)
        setAccessibilityEnabled(entry.enabled)
        if entry.checked { setAccessibilityValue(true) }
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    override func updateTrackingAreas() {
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: bounds, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self))
    }

    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        let base = entry.symbol ?? MenuIcons.symbol(for: entry.title) ?? "circle"
        icon.image = NSImage(systemSymbolName: entry.checked ? base + ".fill" : base, accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: newSize.width * T.menuQuickIconRatio, weight: .regular))
    }

    override func mouseEntered(with event: NSEvent) {
        hovering = entry.enabled
        MenuTip.shared.show(toolTip ?? entry.title, under: self)
    }
    override func mouseExited(with event: NSEvent) {
        hovering = false
        MenuTip.shared.hide()
    }
    override func mouseUp(with event: NSEvent) { if entry.enabled { press() } }
    override func viewWillMove(toWindow newWindow: NSWindow?) {
        if newWindow == nil { MenuTip.shared.hide() }
        super.viewWillMove(toWindow: newWindow)
    }

    override func draw(_ dirtyRect: NSRect) {
        guard hovering else { return }
        NSColor.selectedContentBackgroundColor.setFill()
        NSBezierPath(roundedRect: bounds, xRadius: T.menuQuickRadius, yRadius: T.menuQuickRadius).fill()
    }

    private func tint() {
        icon.contentTintColor = !entry.enabled ? .disabledControlTextColor : hovering ? .alternateSelectedControlTextColor : .labelColor
    }

    #if DEBUG
    func debugHover() { hovering = entry.enabled }
    #endif

    func press() {
        MenuTip.shared.hide()
        let menu = enclosingMenuItem?.menu
        let spot = window.map { $0.convertPoint(toScreen: convert(NSPoint(x: bounds.minX, y: bounds.minY), to: nil)) }
        menu?.cancelTracking()
        let entry = entry
        // after the menu has closed: the action may open a dialog, and the list replaces the menu.
        Task { @MainActor in
            guard let rows = entry.submenu else { entry.action(); return }
            let list = makeMenu(rows, find: entry.find, empty: entry.empty)
            list.popUp(positioning: nil, at: spot ?? NSEvent.mouseLocation, in: nil)
        }
    }
}

/// the system's tooltips never show inside an open menu, so the icon-only buttons get their own after the same delay.
@MainActor final class MenuTip {
    static let shared = MenuTip()
    private var panel: NSPanel?
    private let label = NSTextField(labelWithString: "")
    private var timer: Timer?

    func show(_ text: String, under view: NSView) {
        hide()
        let delay = UserDefaults.standard.object(forKey: "NSInitialToolTipDelay") as? Double ?? T.menuTipDelay * 1000
        let t = Timer(timeInterval: delay / 1000, repeats: false) { [weak self, weak view] _ in
            MainActor.assumeIsolated { if let view { self?.present(text, under: view) } }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    func hide() {
        timer?.invalidate()
        timer = nil
        panel?.orderOut(nil)
    }

    private func present(_ text: String, under view: NSView) {
        guard let window = view.window else { return }
        let panel = self.panel ?? make()
        label.stringValue = text
        label.sizeToFit()
        let size = NSSize(width: label.frame.width + T.menuTipPadX * 2, height: label.frame.height + T.menuTipPadY * 2)
        label.setFrameOrigin(NSPoint(x: T.menuTipPadX, y: T.menuTipPadY))
        let anchor = window.convertToScreen(view.convert(view.bounds, to: nil))
        panel.setFrame(NSRect(x: anchor.midX - size.width / 2, y: anchor.minY - size.height - T.menuTipGap, width: size.width, height: size.height), display: true)
        panel.orderFront(nil)
    }

    private func make() -> NSPanel {
        let panel = NSPanel(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: true)
        panel.level = .popUpMenu + 1
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.ignoresMouseEvents = true
        let body = NSVisualEffectView()
        body.material = .toolTip
        body.state = .active
        body.wantsLayer = true
        body.layer?.cornerRadius = T.menuTipRadius
        label.font = .toolTipsFont(ofSize: 0)
        label.textColor = .labelColor
        body.addSubview(label)
        panel.contentView = body
        self.panel = panel
        return panel
    }
}

/// takes the keyboard when the submenu opens; Return picks the first match. system label colours, so it reads in a
/// light menu and a dark one.
final class MenuFindField: NSView, NSMenuDelegate, NSTextFieldDelegate {
    /// the field editor paints an opaque background while typing unless it's told not to once it's in place.
    private final class ClearField: NSTextField {
        override func becomeFirstResponder() -> Bool {
            let became = super.becomeFirstResponder()
            clearEditor()
            return became
        }
        override func textDidChange(_ notification: Notification) { super.textDidChange(notification); clearEditor() }
        private func clearEditor() {
            guard let editor = currentEditor() as? NSTextView else { return }
            editor.drawsBackground = false
            editor.backgroundColor = .clear
            (editor.superview as? NSClipView)?.drawsBackground = false
        }
    }

    let field: NSTextField = ClearField()
    /// the search field's ✕ (`SearchClearButton`), only while there's text.
    private let clear = NSButton()
    var filterable: [NSMenuItem] = []
    var emptyItem: NSMenuItem?

    init(placeholder: String) {
        super.init(frame: NSRect(x: 0, y: 0, width: T.submenuW, height: T.menuFindH + T.menuFindSpace))
        let box = NSView()
        box.wantsLayer = true
        box.layer?.cornerRadius = T.menuFindRadius
        box.layer?.cornerCurve = .continuous
        let glyph = NSImageView(image: NSImage(systemSymbolName: "magnifyingglass", accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: T.menuFindGlyph, weight: .regular)) ?? NSImage())
        glyph.contentTintColor = .secondaryLabelColor
        field.placeholderAttributedString = NSAttributedString(string: placeholder, attributes: [
            .foregroundColor: NSColor.tertiaryLabelColor, .font: NSFont.menuFont(ofSize: 0)])
        field.font = .menuFont(ofSize: 0)
        field.textColor = .labelColor
        field.isBordered = false
        field.isBezeled = false
        field.drawsBackground = false
        field.backgroundColor = .clear
        field.focusRingType = .none
        field.usesSingleLineMode = true
        field.cell?.isScrollable = true
        field.delegate = self
        clear.image = NSImage(systemSymbolName: "xmark.circle.fill", accessibilityDescription: L("Clear Search"))?
            .withSymbolConfiguration(.init(pointSize: T.searchClearSM, weight: .regular))
        clear.isBordered = false
        clear.contentTintColor = .tertiaryLabelColor
        clear.toolTip = L("Clear Search")
        clear.refusesFirstResponder = true
        clear.target = self
        clear.action = #selector(clearFind)
        clear.isHidden = true
        self.box = box
        addSubview(box)
        for view in [glyph, field, clear] { box.addSubview(view) }
        for view in [box, glyph, field, clear] { view.translatesAutoresizingMaskIntoConstraints = false }
        NSLayoutConstraint.activate([
            box.leadingAnchor.constraint(equalTo: leadingAnchor, constant: T.menuContentInset),
            box.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -T.menuContentInset),
            box.centerYAnchor.constraint(equalTo: centerYAnchor),
            box.heightAnchor.constraint(equalToConstant: T.menuFindH),
            glyph.leadingAnchor.constraint(equalTo: box.leadingAnchor, constant: T.menuFindPad),
            glyph.centerYAnchor.constraint(equalTo: box.centerYAnchor),
            field.leadingAnchor.constraint(equalTo: glyph.trailingAnchor, constant: T.menuFindGap),
            field.trailingAnchor.constraint(equalTo: clear.leadingAnchor, constant: -T.menuFindGap),
            field.centerYAnchor.constraint(equalTo: box.centerYAnchor),
            clear.trailingAnchor.constraint(equalTo: box.trailingAnchor, constant: -T.menuFindPad),
            clear.centerYAnchor.constraint(equalTo: box.centerYAnchor),
        ])
        glyph.setContentHuggingPriority(.required, for: .horizontal)
        clear.setContentHuggingPriority(.required, for: .horizontal)
    }

    @objc private func clearFind() {
        field.stringValue = ""
        apply()
        window?.makeFirstResponder(field)
    }

    private var box: NSView?

    // layer colours don't follow the appearance on their own.
    override func viewDidChangeEffectiveAppearance() { tint() }
    override func viewDidMoveToWindow() { tint() }
    private func tint() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            box?.layer?.backgroundColor = NSColor.labelColor.withAlphaComponent(CGFloat(T.row.a)).cgColor
        }
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    func menuWillOpen(_ menu: NSMenu) {
        field.stringValue = ""
        apply()
        // The menu's window exists only once it's tracking, so focus the field then.
        RunLoop.main.perform(inModes: [.eventTracking, .default]) { [weak self] in
            guard let self else { return }
            self.window?.makeFirstResponder(self.field)
        }
    }

    func controlTextDidChange(_ obj: Notification) { apply() }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard selector == #selector(NSResponder.insertNewline(_:)), !field.stringValue.isEmpty,
              let first = filterable.first(where: { !$0.isHidden }), let menu = first.menu else { return false }
        menu.cancelTracking()
        if let index = menu.items.firstIndex(of: first) { menu.performActionForItem(at: index) }
        return true
    }

    private func apply() {
        let needle = field.stringValue.trimmingCharacters(in: .whitespaces).lowercased()
        var any = false
        for item in filterable {
            let title = item.attributedTitle?.string ?? item.title
            item.isHidden = !needle.isEmpty && !title.lowercased().contains(needle)
            any = any || !item.isHidden
        }
        emptyItem?.isHidden = any || filterable.isEmpty && needle.isEmpty
        clear.isHidden = field.stringValue.isEmpty
    }

    #if DEBUG
    /// For checks: types into the filter as the keyboard would.
    func debugType(_ text: String) { field.stringValue = text; apply() }
    #endif
}

/// The song row menu.
enum SongMenu {
    static func info(_ target: InfoTarget, shell: ShellModel) -> MenuEntry {
        MenuEntry(title: L("Get Info"), symbol: "info.circle", shortcut: "i") { shell.openInfo(target) }
    }

    /// Add to Playlist: a submenu with a filter, New Playlist (named `name` to start), then the user's own playlists,
    /// most recently changed first.
    static func addToPlaylist(_ songs: [SongRow], name: String, title: String = L("Add to Playlist"), shell: ShellModel) -> MenuEntry {
        guard !songs.isEmpty else { return MenuEntry(title: title, enabled: false, reason: L("No songs to add"), symbol: "text.badge.plus") }
        var rows = [MenuEntry(title: L("New Playlist"), symbol: "plus") { shell.openNewPlaylist(songs: songs, name: name) }]
        rows += shell.library.playlistTargets().map { target in
            MenuEntry(title: target.name, filterable: true) { shell.addSongs(songs, toPlaylist: target) }
        }
        return MenuEntry(title: title, submenu: rows, find: L("Find a playlist"), empty: L("No playlists match"), symbol: "text.badge.plus")
    }

    /// for rows of a playlist the user can edit.
    static func remove(count: Int, action: @escaping () -> Void) -> MenuEntry {
        MenuEntry(title: count > 1 ? L("Remove {n} from Playlist", ["n": fmtInt(count)]) : L("Remove from Playlist"), symbol: "minus.circle", action: action)
    }

    /// Remove from Queue. The X and Delete do the same.
    static func removeFromQueue(count: Int, action: @escaping () -> Void) -> MenuEntry {
        MenuEntry(title: count > 1 ? L("Remove {n} from Queue", ["n": fmtInt(count)]) : L("Remove from Queue"), symbol: "minus.circle", action: action)
    }

    /// for several songs each acts on all of them.
    static func quick(_ songs: [SongRow], shell: ShellModel) -> [MenuEntry] {
        let library = shell.library
        let one = songs.count == 1
        let favorite = songs.allSatisfy { library.isFavorite(song: $0) }
        let downloaded = songs.allSatisfy { library.isDownloaded(song: $0) }
        let downloading = !downloaded && songs.contains { library.isDownloading(song: $0) } && songs.allSatisfy { library.isDownloaded(song: $0) || library.isDownloading(song: $0) }
        // A song a downloaded playlist or album holds stays until that collection's download is turned off.
        let singles = library.synced?.singleIDs()
        let removable = singles.map { ids in songs.contains { ids.contains($0.id) } } ?? true
        let download = downloaded
            ? MenuEntry(title: one ? L("Remove Download") : L("Remove Downloads"), enabled: removable, reason: L("Kept by a downloaded playlist or album"), symbol: "arrow.down.circle", quick: true) { library.setDownloaded(songs: songs, false) }
            : downloading
            ? MenuEntry(title: L("Cancel Download"), symbol: "xmark", quick: true) { library.setDownloaded(songs: songs, false) }
            : MenuEntry(title: one ? L("Download") : L("Download Selected"), symbol: "arrow.down", quick: true) { library.setDownloaded(songs: songs, true) }
        var add = addToPlaylist(songs, name: one ? songs[0].title : "", title: one ? L("Add to Playlist") : L("Add {n} to Playlist", ["n": fmtInt(songs.count)]), shell: shell)
        add.symbol = "plus"
        add.quick = true
        return [
            MenuEntry(title: L("Favorite"), checked: favorite, symbol: "heart", quick: true) {
                for song in songs where library.isFavorite(song: song) == favorite { library.toggleFavorite(song: song) }
            },
            download,
            MenuEntry(title: one ? L("Play Next") : L("Play {n} Next", ["n": fmtInt(songs.count)]), symbol: "arrow.turn.up.right", quick: true) { songs.reversed().forEach { shell.player.playNext($0) }; RowFlash.shared.flash(songs.map(\.id)) },
            add,
        ]
    }

    /// what a double-click on the row does (`action`); without one, a queued song is jumped to, anything else plays as
    /// its own list. on the current song it is Pause, or Play to resume.
    static func play(_ songs: [SongRow], shell: ShellModel, action: (() -> Void)? = nil) -> MenuEntry {
        let player = shell.player
        if player.station == nil, songs.count == 1, let song = songs.first, player.current?.id == song.id {
            return MenuEntry(title: player.isPlaying ? L("Pause") : L("Play"), symbol: player.isPlaying ? "pause" : "play") { player.togglePlayPause() }
        }
        return MenuEntry(title: L("Play"), enabled: !songs.isEmpty, symbol: "play") {
            if let action { return action() }
            if songs.count == 1, let song = songs.first {
                if let offset = (0..<player.upcomingCount).first(where: { player.upcoming(at: $0)?.song.id == song.id }) { return player.jump(toUpcoming: offset) }
            }
            shell.play(songs, at: 0, from: nil)
        }
    }

    static func single(_ song: SongRow, shell: ShellModel, remove: MenuEntry? = nil, play: (() -> Void)? = nil) -> [MenuEntry] {
        let library = shell.library
        var entries = quick([song], shell: shell) + [
            Self.play([song], shell: shell, action: play),
            .divider,
            MenuEntry(title: L("Add to Queue")) { shell.player.addToQueue(song); RowFlash.shared.flash([song.id]) },
            MenuEntry(title: L("Song Radio"), enabled: !library.knownNoRadio(song)) { shell.startSongRadio(song) },
            .divider,
            addToPlaylist([song], name: song.title, shell: shell),
        ]
        if let remove { entries.append(remove) }
        entries.append(.divider)
        entries.append(MenuEntry(title: L("Go to Album")) { shell.goToAlbum(of: song) })
        entries.append(artistEntry(song, shell: shell))
        entries.append(shell.shareEntry(songs: [song]))
        entries.append(info(.song(song.id), shell: shell))
        return entries
    }

    /// one artist goes straight there; several credited artists are listed in the song's own order.
    private static func artistEntry(_ song: SongRow, shell: ShellModel) -> MenuEntry {
        let targets = shell.artistTargets(of: song)
        guard targets.count > 1 else {
            return MenuEntry(title: L("Go to Artist"), enabled: !targets.isEmpty) { if let target = targets.first { shell.goToArtist(target) } }
        }
        return MenuEntry(title: L("Go to Artist"), submenu: targets.map { target in
            MenuEntry(title: target.name) { shell.goToArtist(target) }
        })
    }

    static func several(_ songs: [SongRow], shell: ShellModel, remove: MenuEntry? = nil) -> [MenuEntry] {
        quick(songs, shell: shell) + [
            play(songs, shell: shell),
            .divider,
            MenuEntry(title: L("Add {n} to Queue", ["n": fmtInt(songs.count)]), symbol: "text.line.first.and.arrowtriangle.forward") { songs.forEach { shell.player.addToQueue($0) }; RowFlash.shared.flash(songs.map(\.id)) },
            .divider,
            addToPlaylist(songs, name: "", title: L("Add {n} to Playlist", ["n": fmtInt(songs.count)]), shell: shell),
        ] + (remove.map { [$0] } ?? []) + [
            .divider,
            shell.shareEntry(songs: songs),
        ]
    }
}

/// editing and deleting only on playlists the user owns (admins count).
enum PlaylistMenu {
    static func entries(_ playlist: PlaylistRow, shell: ShellModel) -> [MenuEntry] {
        let none = playlist.songCount == 0
        var items = [
            MenuEntry(title: L("Play"), enabled: !none, reason: L("No songs to play"), symbol: "play") { shell.playPlaylist(playlist, shuffle: false) },
            MenuEntry(title: L("Shuffle"), enabled: !none, reason: L("No songs to play"), symbol: "shuffle") { shell.playPlaylist(playlist, shuffle: true) },
            .divider,
        ]
        if let synced = shell.library.synced {
            let on = synced.status(.playlist, playlist.id) != nil
            items.append(MenuEntry(title: on ? L("Remove Download") : L("Download"), enabled: !none, reason: L("No songs to download"), symbol: on ? "xmark" : "arrow.down") {
                shell.toggleCollectionDownload(.playlist, id: playlist.id, name: playlist.name)
            })
        }
        items += [
            shell.shareEntry(playlist: playlist),
            SongMenu.addToPlaylist(shell.library.playlistSongsNow(id: playlist.id), name: playlist.name, shell: shell),
            SongMenu.info(.playlist(playlist.id), shell: shell),
        ]
        if let pin = shell.pinEntry(playlist.id) { items.append(pin) }
        if playlist.isOwned { items += [.divider, editDetails(playlist, shell: shell), delete(playlist, shell: shell)] }
        return items
    }

    static func editDetails(_ playlist: PlaylistRow, shell: ShellModel) -> MenuEntry {
        MenuEntry(title: L("Edit Details…"), symbol: "pencil") { shell.openEditPlaylist(playlist) }
    }

    static func delete(_ playlist: PlaylistRow, shell: ShellModel) -> MenuEntry {
        MenuEntry(title: L("Delete Playlist…"), symbol: "trash") { shell.askDeletePlaylist(playlist) }
    }
}

/// sits over any label so sort, filter and "more" buttons keep their look.
struct MenuHitArea: NSViewRepresentable {
    /// for buttons at the window's right.
    var trailing = false
    /// the open menu follows changes to what `entries` reads.
    var live = false
    /// runs as the menu opens, before `entries`.
    var opened: (() -> Void)? = nil
    let entries: () -> [MenuEntry]
    /// drawn inside the view, where Core Animation can press it.
    var glyph: AnyView? = nil
    var press = false
    /// the held fill's diameter, where the surface has one.
    var fill: CGFloat? = nil

    func makeNSView(context: Context) -> MenuOpeningView { MenuOpeningView() }

    func updateNSView(_ view: MenuOpeningView, context: Context) {
        view.entries = entries
        view.live = live
        view.opened = opened
        view.trailing = trailing
        view.press = press
        view.fill = fill
        view.show(glyph.map { AnyView($0.environment(\.self, context.environment)) })
    }
}

final class MenuOpeningView: NSView {
    var entries: () -> [MenuEntry] = { [] }
    var live = false
    var opened: (() -> Void)?
    var trailing = false
    var press = false
    var fill: CGFloat?
    private var host: NSHostingView<AnyView>?
    private let fillLayer = CALayer()

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? { frame.contains(point) ? self : nil }

    func show(_ glyph: AnyView?) {
        guard let glyph else { host?.removeFromSuperview(); host = nil; return }
        wantsLayer = true
        if let host { host.rootView = glyph; return }
        let made = NSHostingView(rootView: glyph)
        made.sizingOptions = []
        made.wantsLayer = true
        made.frame = bounds
        made.autoresizingMask = [.width, .height]
        fillLayer.backgroundColor = CGColor(srgbRed: T.activeFill.r, green: T.activeFill.g, blue: T.activeFill.b, alpha: T.activeFill.a)
        fillLayer.opacity = 0
        layer?.addSublayer(fillLayer)
        addSubview(made)
        host = made
    }

    override func layout() {
        super.layout()
        host?.frame = bounds
        guard let fill else { return }
        fillLayer.frame = CGRect(x: bounds.midX - fill / 2, y: bounds.midY - fill / 2, width: fill, height: fill)
        fillLayer.cornerRadius = fill / 2
    }

    override func mouseDown(with event: NSEvent) {
        if press { playPress() }
        open()
    }

    /// squash, past its size, and back; the fill shows through it. run by the window server, so it plays over the open menu.
    private func playPress() {
        guard let layer = host?.layer else { return }
        let total = T.pressIn + 2 * T.pressOut
        let times = [0, T.pressIn / total, (T.pressIn + T.pressOut) / total, 1].map { NSNumber(value: $0) }
        func around(_ scale: CGFloat) -> NSValue {
            let mid = CGPoint(x: bounds.midX, y: bounds.midY)
            var t = CATransform3DMakeTranslation(mid.x, mid.y, 0)
            t = CATransform3DScale(t, scale, scale, 1)
            return NSValue(caTransform3D: CATransform3DTranslate(t, -mid.x, -mid.y, 0))
        }
        let squash = CAKeyframeAnimation(keyPath: "transform")
        squash.values = [1, T.pressSquash, T.pressOver, 1].map(around)
        squash.keyTimes = times
        squash.timingFunctions = [.init(name: .easeOut), .init(name: .easeInEaseOut), .init(name: .easeInEaseOut)]
        squash.duration = total
        layer.add(squash, forKey: "press")
        guard fill != nil else { return }
        let fade = CAKeyframeAnimation(keyPath: "opacity")
        fade.values = [0, 1, 1, 0]
        fade.duration = total + T.activeFillFade
        fade.keyTimes = [0, NSNumber(value: T.pressIn / fade.duration), NSNumber(value: total / fade.duration), 1]
        fillLayer.add(fade, forKey: "fill")
    }

    /// below the button, or above when it fits only or better there, so macOS doesn't clip it with a scroll arrow.
    func open() {
        opened?()
        let items = entries()
        guard !items.isEmpty else { return }
        let menu = makeMenu(items)
        MenuOpeningView.lastOpened = menu
        let follower = live ? LiveMenu(menu, entries: entries, shown: items) : nil
        follower?.follow()
        // returns when the menu closes.
        menu.popUp(positioning: nil, at: placement(for: menu.size).point, in: self)
        follower?.close()
    }

    /// its top-left corner in this view's coordinates, and whether that's above the button.
    func placement(for size: NSSize) -> (point: NSPoint, above: Bool) {
        let gap: CGFloat = 4
        var left = trailing ? bounds.maxX - size.width : 0
        var above = false
        if let window, let visible = window.screen?.visibleFrame {
            let button = window.convertToScreen(convert(bounds, to: nil))
            let below = button.minY - gap - visible.minY
            let over = visible.maxY - (button.maxY + gap)
            above = size.height > below && over > below
            left = min(max(button.minX + left, visible.minX), visible.maxX - size.width) - button.minX
        }
        let top: CGFloat = isFlipped ? (above ? bounds.minY - gap - size.height : bounds.maxY + gap) : (above ? bounds.maxY + gap + size.height : bounds.minY - gap)
        return (NSPoint(x: left, y: top), above)
    }

    /// for checks.
    nonisolated(unsafe) static weak var lastOpened: NSMenu?
}

struct MenuIconButton: View {
    let symbol: String
    let label: String
    var size: CGFloat = T.px(17)
    var width: CGFloat = T.iconW
    var height: CGFloat = T.iconButtonH
    var tint: Color? = nil
    /// the button dims like a disabled one and says why.
    var disabledReason: String? = nil
    /// see `MenuHitArea`.
    var live = false
    var opened: (() -> Void)? = nil
    let entries: () -> [MenuEntry]
    @State private var hovering = false
    @Environment(\.isSnapshot) private var isSnapshot
    @Environment(\.springPress) private var spring
    @Environment(\.activeFills) private var activeFills

    var body: some View {
        let disabled = disabledReason != nil
        let glyph = Image(systemName: symbol)
            .font(.system(size: size))
            .foregroundStyle(tint.map { AnyShapeStyle($0) } ?? AnyShapeStyle(.foreground))
            .frame(width: width, height: height)
            .modifier(HoverBrighten(on: !disabled && (hovering || HoverDebug.forced(label))))
        Group {
            if isSnapshot || disabled {
                glyph
            } else if spring && !Motion.isReduced {
                // the glyph lives in the hit area's own layer, so its press animates while the menu tracks.
                MenuHitArea(live: live, opened: opened, entries: entries, glyph: AnyView(glyph), press: true, fill: activeFills ? size * T.activeFillSize : nil)
                    .frame(width: width, height: height)
            } else {
                glyph.overlay { MenuHitArea(live: live, opened: opened, entries: entries) }
            }
        }
        .opacity(disabled ? 0.3 : (hovering || spring ? 1 : T.iconRest))
        .contentShape(Capsule())
        .onHover { hovering = $0 }
        .help(disabledReason.map { "\(label): \($0)" } ?? label)
        .accessibilityLabel(label)
        .accessibilityAddTraits(.isButton)
    }
}

struct MenuTextButton: View {
    let title: String
    let entries: () -> [MenuEntry]
    @State private var hovering = false
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        HStack(spacing: T.px(6)) {
            Text(title).font(.navia(T.textMD, .medium))
            Image(systemName: "chevron.up.chevron.down").font(.system(size: T.px(11), weight: .bold))
        }
        .padding(.horizontal, T.px(12))
        .frame(height: T.iconButtonH)
        .background(HoverFillBackground(on: hovering || HoverDebug.forced(title)))
        .contentShape(Capsule())
        .overlay { if !isSnapshot { MenuHitArea(entries: entries) } }
        .onHover { hovering = $0 }
    }
}

/// A menu row with a switch at its end, such as Private Mode. A view-based item doesn't end the menu's tracking,
/// so the menu stays open when it flips.
final class MenuSwitchRow: NSView {
    private let toggle = NSSwitch()
    private let action: () -> Void

    init(title: String, image: NSImage?, on: Bool, action: @escaping () -> Void) {
        self.action = action
        super.init(frame: NSRect(x: 0, y: 0, width: 240, height: 28))
        // Follow the menu's width, so the switch ends at the same inset as the other rows.
        autoresizingMask = [.width]
        let icon = NSImageView()
        icon.image = image
        icon.contentTintColor = .labelColor
        let label = NSTextField(labelWithString: title)
        label.font = .menuFont(ofSize: 0)
        toggle.controlSize = .mini
        toggle.state = on ? .on : .off
        toggle.target = self
        toggle.action = #selector(flipped)
        for view in [icon, label, toggle] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        NSLayoutConstraint.activate([
            icon.leadingAnchor.constraint(equalTo: leadingAnchor, constant: T.menuContentInset),
            icon.centerYAnchor.constraint(equalTo: centerYAnchor),
            label.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 6),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
            toggle.leadingAnchor.constraint(greaterThanOrEqualTo: label.trailingAnchor, constant: 18),
            toggle.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -T.menuContentInset),
            toggle.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
        setAccessibilityElement(false)
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    /// A click anywhere on the row flips the switch.
    override func mouseUp(with event: NSEvent) {
        toggle.state = toggle.state == .on ? .off : .on
        flipped()
    }

    @objc private func flipped() { action() }

    #if DEBUG
    /// For checks: flips it as a click on the row would.
    func debugFlip() { mouseUp(with: NSEvent()) }
    var debugOn: Bool { toggle.state == .on }
    /// For checks: the switch's inset from the row's right edge, and the row's width.
    var debugInsets: (right: CGFloat, width: CGFloat) { (bounds.maxX - toggle.frame.maxX, bounds.width) }
    #endif
}
