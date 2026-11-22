import AppKit
import SwiftUI

/// Selected rows of a small SwiftUI song list, with the same rules as `SongsTable`. Rows are identified by
/// position, because one song can appear twice.
@Observable final class RowSelection {
    private(set) var selected: Set<Int> = []
    @ObservationIgnored private var anchor: Int?
    @ObservationIgnored private var monitor: Any?

    func click(_ index: Int, modifiers: NSEvent.ModifierFlags) {
        if modifiers.contains(.shift), let anchor {
            let range = min(anchor, index)...max(anchor, index)
            set(Set(range))
            return
        }
        if modifiers.contains(.control) || modifiers.contains(.command) {
            var next = selected
            if next.remove(index) == nil { next.insert(index) }
            set(next)
        } else {
            set([index])
        }
        anchor = index
    }

    /// Right-clicking a row outside the selection makes it the selection.
    func selectForMenu(_ index: Int) {
        guard !selected.contains(index) else { return }
        set([index])
        anchor = index
    }

    func clear() { set([]); anchor = nil }

    /// Selects exactly these rows, as when a move carries the selection to their new places.
    func select(_ rows: Set<Int>) {
        set(rows)
        anchor = rows.min()
    }

    private func set(_ new: Set<Int>) {
        selected = new
        if new.isEmpty {
            if let monitor { NSEvent.removeMonitor(monitor) }
            monitor = nil
        } else if monitor == nil {
            // Clicking outside the list clears the selection. Menus track their own events, so they don't reach this.
            monitor = NSEvent.addLocalMonitorForEvents(matching: .leftMouseDown) { [weak self] event in
                guard let self, let root = event.window?.contentView?.superview else { return event }
                if let hit = root.hitTest(root.convert(event.locationInWindow, from: nil)) as? ClickCatchingView, hit.selection === self { return event }
                self.clear()
                return event
            }
        }
    }

    isolated deinit { if let monitor { NSEvent.removeMonitor(monitor) } }
}

/// Catches row clicks under a SwiftUI row, so clicks act at once and the row's own buttons stay on top.
struct ClickCatcher: NSViewRepresentable {
    let selection: RowSelection?
    var onClick: (NSEvent) -> Void
    var onMenu: (NSEvent) -> NSMenu?
    var onReturn: () -> Void
    var hold: RowHold? = nil
    var onDelete: (() -> Void)? = nil

    func makeNSView(context: Context) -> ClickCatchingView { ClickCatchingView() }

    func updateNSView(_ view: ClickCatchingView, context: Context) {
        view.selection = selection
        view.onClick = onClick
        view.onMenu = onMenu
        view.onReturn = onReturn
        view.hold = hold
        view.onDelete = onDelete
    }
}

/// Press-and-hold on a reorderable row: holding still 180 ms or moving 6 px lifts it. Quick, double, modifier and
/// right clicks keep their usual meaning.
struct RowHold {
    static let holdSeconds = 0.18
    static let movePoints: CGFloat = 6
    /// True when a plain click shouldn't select at once, because the row is part of a multi-selection that may be
    /// dragged. The click then selects on release if nothing lifted.
    var deferClick: () -> Bool
    /// The row lifts. The drag owns the pointer until release or Esc.
    var lift: (NSEvent) -> Void
}

final class ClickCatchingView: NSView {
    weak var selection: RowSelection?
    var onClick: (NSEvent) -> Void = { _ in }
    var onMenu: (NSEvent) -> NSMenu? = { _ in nil }
    var onReturn: () -> Void = {}
    var hold: RowHold?
    var onDelete: (() -> Void)?
    private var press: (event: NSEvent, deferred: Bool, timer: Timer)?

    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override func mouseDown(with event: NSEvent) {
        if TitlebarDoubleClick.handleInTopBar(event) || TitlebarDoubleClick.movesWindow(event) { return }
        window?.makeFirstResponder(self)
        let plain = event.modifierFlags.intersection([.command, .control, .shift, .option]).isEmpty
        guard let hold, plain, event.clickCount == 1 else { onClick(event); return }
        let deferred = hold.deferClick()
        if !deferred { onClick(event) }
        let timer = Timer.scheduledTimer(withTimeInterval: RowHold.holdSeconds, repeats: false) { [weak self] _ in
            MainActor.assumeIsolated { self?.liftNow() }
        }
        press = (event, deferred, timer)
    }
    override func mouseDragged(with event: NSEvent) {
        guard let press else { return }
        let a = press.event.locationInWindow, b = event.locationInWindow
        if hypot(b.x - a.x, b.y - a.y) >= RowHold.movePoints { liftNow() }
    }
    override func mouseUp(with event: NSEvent) {
        guard let press else { return }
        press.timer.invalidate()
        self.press = nil
        if press.deferred { onClick(press.event) }
    }
    private func liftNow() {
        guard let press, let hold else { return }
        press.timer.invalidate()
        self.press = nil
        hold.lift(press.event)
    }
    override func rightMouseDown(with event: NSEvent) {
        guard let menu = onMenu(event) else { return }
        NSMenu.popUpContextMenu(menu, with: event, for: self)
    }
    /// Control-click selects instead of opening a menu.
    override func menu(for event: NSEvent) -> NSMenu? {
        event.type == .leftMouseDown ? nil : onMenu(event)
    }
    override func keyDown(with event: NSEvent) {
        if event.keyCode == 36 || event.keyCode == 76 { onReturn() }
        else if (event.keyCode == 51 || event.keyCode == 117), let onDelete { onDelete() }
        else { super.keyDown(with: event) }
    }
}

/// shared by the mini lists and the queue.
struct SelectableRow<Lead: View, Text1: View, Trailing: View>: View {
    let index: Int
    let selection: RowSelection
    let height: CGFloat
    let radius: CGFloat
    let hoverFill: Color
    let selectedFill: Color
    let leadWidth: CGFloat
    let onPlay: (Int) -> Void
    let menu: (Int) -> [MenuEntry]
    var onReturn: () -> Void = {}
    var leadPad: CGFloat = 0
    var trailPad: CGFloat = 0
    var hold: RowHold? = nil
    var onDelete: (() -> Void)? = nil
    /// the song the row shows, so it lights when that song is queued or added to a playlist.
    var flashID: String? = nil
    @ViewBuilder var lead: Lead
    @ViewBuilder var text: Text1
    @ViewBuilder var trailing: Trailing
    @State private var hovering = false
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        let selected = selection.selected.contains(index)
        ZStack {
            if !isSnapshot {
                ClickCatcher(selection: selection, onClick: { event in
                    let flags = event.modifierFlags.intersection([.command, .control, .shift])
                    if event.clickCount >= 2 && flags.isEmpty { onPlay(index); return }
                    selection.click(index, modifiers: flags)
                }, onMenu: { _ in
                    selection.selectForMenu(index)
                    return makeMenu(menu(index))
                }, onReturn: onReturn, hold: hold, onDelete: onDelete)
            }
            HStack(spacing: T.px(12)) {
                ZStack {
                    lead
                    if hovering {
                        Button { onPlay(index) } label: {
                            Image(systemName: "play.fill")
                                .font(.system(size: T.px(15)))
                                .foregroundStyle(T.tOnArt.color)
                                .shadow(T.hoverPlayShadow)
                                .frame(width: leadWidth, height: leadWidth)
                                .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .help(L("Play"))
                    }
                }
                .frame(width: leadWidth, height: leadWidth)
                // The plain text stays out of hit testing, so its links can take clicks.
                text
                trailing
            }
            .padding(.leading, leadPad)
            .padding(.trailing, trailPad)
        }
        .frame(height: height)
        .background(RoundedRectangle(cornerRadius: radius, style: .continuous).fill(selected ? selectedFill : hovering ? hoverFill : .clear))
        .background { if let flashID { RowFlashFill(id: flashID, radius: radius) } }
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
    }
}

/// click to cancel.
struct DownloadProgressButton: View {
    let fraction: Double
    let label: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            ZStack {
                Circle().stroke(T.t2.color.opacity(0.35), lineWidth: 2)
                Circle().trim(from: 0, to: max(0.03, fraction)).stroke(T.t1.color, style: StrokeStyle(lineWidth: 2, lineCap: .round)).rotationEffect(.degrees(-90))
                Image(systemName: "xmark").font(.system(size: T.px(9), weight: .bold)).foregroundStyle(T.t2.color)
            }
            .frame(width: T.px(20), height: T.px(20))
            .frame(height: T.iconButtonH)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(label)
        .accessibilityLabel(label)
    }
}

/// A titled block of two-column song rows, lined up with shelf headings. Shows nothing without songs.
struct SongShelf: View {
    let title: String
    var note: String? = nil
    let songs: [SongRow]
    let shell: ShellModel
    var showAlbum = true

    var body: some View {
        if !songs.isEmpty {
            VStack(alignment: .leading, spacing: T.shelfHeadGap) {
                VStack(alignment: .leading, spacing: T.px(2)) {
                    SectionTitle(title: title)
                    if let note { Text(note).font(.navia(T.textSM)).foregroundStyle(T.t2.color) }
                }
                .padding(.leading, T.gutter)
                SongColumnsView(songs: songs, shell: shell, showAlbum: showAlbum, source: PlaySource(name: title))
            }
        }
    }
}

struct SongColumnsView: View {
    let songs: [SongRow]
    let shell: ShellModel
    var showAlbum = true
    var one = false
    var gutter: CGFloat = T.gutter
    /// Play a song as a radio from itself instead of the list (Search).
    var playsAsRadio = false
    /// What the rows play from when it isn't the page itself (a shelf's name).
    var source: PlaySource? = nil
    @State private var selection = RowSelection()
    @Environment(\.pageContext) private var ctx

    var body: some View {
        let columns = one || ctx.cssWidth <= 1000 ? 1 : 2
        let rows = (songs.count + columns - 1) / columns
        VStack(spacing: 0) {
            ForEach(0..<rows, id: \.self) { row in
                HStack(spacing: T.px(38)) {
                    ForEach(row * columns..<(row + 1) * columns, id: \.self) { index in
                        if index < songs.count {
                            MiniSongRow(song: songs[index], index: index, shell: shell, showAlbum: showAlbum, selection: selection,
                                        play: { play($0) },
                                        menu: { menuEntries(at: $0) }, hold: hold(index))
                                .frame(maxWidth: .infinity)
                        } else {
                            Color.clear.frame(maxWidth: .infinity, maxHeight: 1)
                        }
                    }
                }
            }
        }
        .padding(.horizontal, gutter)
    }

    private func play(_ index: Int) { playsAsRadio ? shell.playAsRadio(songs[index]) : shell.play(songs, at: index, from: source) }

    private func menuEntries(at index: Int) -> [MenuEntry] {
        if selection.selected.count > 1, selection.selected.contains(index) {
            return SongMenu.several(selection.selected.sorted().map { songs[$0] }, shell: shell)
        }
        return SongMenu.single(songs[index], shell: shell, play: { play(index) })
    }

    /// A held row, or the selection it's in, lifts out as songs to drop on a playlist or the queue. These lists don't
    /// reorder.
    private func hold(_ index: Int) -> RowHold {
        RowHold(deferClick: { [selection] in selection.selected.count > 1 && selection.selected.contains(index) },
                lift: { [selection, songs, shell] event in
                    let rows = selection.selected.count > 1 && selection.selected.contains(index) ? selection.selected.sorted() : [index]
                    if rows.count == 1 { selection.select([index]) }
                    guard let view = event.window?.contentView else { return }
                    SongDragSource.shared.begin(rows.filter { $0 < songs.count }.map { songs[$0] }, fromQueue: false, event: event, in: view, shell: shell)
                })
    }
}

private struct MiniSongRow: View {
    let song: SongRow
    let index: Int
    let shell: ShellModel
    let showAlbum: Bool
    let selection: RowSelection
    let play: (Int) -> Void
    let menu: (Int) -> [MenuEntry]
    let hold: RowHold

    var body: some View {
        SelectableRow(index: index, selection: selection, height: T.miniRowH, radius: T.rMD, hoverFill: T.row.color,
                      selectedFill: shell.theme.rowSelected.color, leadWidth: T.thumb,
                      onPlay: play, menu: menu, onReturn: { play(index) }, leadPad: T.px(5), trailPad: T.px(12), hold: hold, flashID: song.id) {
            ArtworkView(url: song.artworkURL, pixels: Int(T.thumb * 2))
                .frame(width: T.thumb, height: T.thumb)
                .clipShape(RoundedRectangle(cornerRadius: T.rSM, style: .continuous))
                .allowsHitTesting(false)
        } text: {
            VStack(alignment: .leading, spacing: 0) {
                TitledText(title: song.title, explicit: song.isExplicit, size: T.textMD, weight: .medium)
                    .foregroundStyle((song.id == shell.player.current?.id ? shell.theme.accentSoft : T.t1).color)
                    .allowsHitTesting(false)
                LinkedText(pieces: subtitle, shell: shell) { selection.click(index, modifiers: $0) }
                    .font(.navia(T.textMD)).foregroundStyle(T.t2.color)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Text(clock(Double(song.duration))).font(.navia(T.textMD).monospacedDigit()).foregroundStyle(T.t2.color).padding(.trailing, T.px(4))
                .allowsHitTesting(false)
        } trailing: {
            SongActions(song: song, shell: shell)
        }
    }

    /// "Artist, Artist • Album", or just the artists, each linking to its page.
    private var subtitle: [LinkPiece] {
        guard showAlbum, !song.album.isEmpty else { return LinkPiece.artist(song) }
        return LinkPiece.joined([LinkPiece.artist(song), [LinkPiece.album(song)]], separator: " • ")
    }
}

struct SongActions: View {
    let song: SongRow
    let shell: ShellModel

    var body: some View {
        HStack(spacing: 0) {
            if song.isDownloaded {
                Color.clear.frame(width: T.actionW, height: 1)
            } else if let progress = shell.library.downloadProgress(song: song) {
                DownloadProgressButton(fraction: progress, label: L("Cancel Download")) { shell.library.setDownloaded(songs: [song], false) }
                    .frame(width: T.actionW)
            } else {
                IconButton(symbol: "arrow.down", label: L("Download"), width: T.actionW) { shell.library.setDownloaded(songs: [song], true) }
            }
            IconButton(symbol: "heart", label: L("Favorite"), active: song.isFavorite, width: T.actionW, pop: true) { shell.library.toggleFavorite(song: song) }
            MenuIconButton(symbol: "ellipsis", label: L("More"), width: T.actionW) { SongMenu.single(song, shell: shell) }
        }
        .padding(.trailing, T.px(12))
    }
}
