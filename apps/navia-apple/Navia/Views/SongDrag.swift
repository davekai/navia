import AppKit
import SwiftUI
import UniformTypeIdentifiers

// Songs dragged out of a list onto a playlist or the queue. Song lists are AppKit drag sources writing
// `com.davekai.Navia.songs`, and playlists and the queue are SwiftUI drop destinations for that type.

extension UTType {
    /// Songs dragged within Navia, as their ids one per line. Declared in Info.plist.
    static let naviaSongs = UTType(exportedAs: "com.davekai.Navia.songs")
}

extension NSPasteboard.PasteboardType {
    static let naviaSongs = NSPasteboard.PasteboardType(UTType.naviaSongs.identifier)
}

func playlistRef(_ row: PlaylistRow) -> PlaylistRef { PlaylistRef(id: row.id, name: row.name) }

enum SongDropTarget: Equatable {
    /// A playlist wherever it shows. The user's own regular playlists take songs, and smart or other people's
    /// playlists refuse them.
    case playlist(PlaylistRef, place: String)
    /// The queue tab and the bar's queue button, which add to the end of Next in Queue.
    case queueEnd(String)
    /// at the gap it opens.
    case queue

    /// Tells targets apart, so only the one under the pointer is highlighted.
    var key: String {
        switch self {
        case .playlist(let p, let place): "playlist-\(place):" + p.id
        case .queueEnd(let place): "queue-end:" + place
        case .queue: "queue"
        }
    }
}

@MainActor @Observable final class SongDragModel {
    /// The songs of the current or last drag. A drop reads them after the session has ended.
    private(set) var songs: [SongRow] = []
    private(set) var active = false
    /// songs from the queue don't drop back onto it.
    private(set) var fromQueue = false
    /// The highlighted target under the pointer, by `SongDropTarget.key`.
    private(set) var over: String?
    /// The pointer is over a target that refuses the songs, so it shows the not-allowed cursor.
    private(set) var refused = false
    /// Playlists that take songs, read when a drag starts.
    @ObservationIgnored private(set) var targets: Set<String> = []
    /// The sidebar's scroll view and a way to scroll it, so it scrolls while songs are carried near its edges.
    /// Scrolling goes through SwiftUI's scroll position, which would otherwise put it back.
    @ObservationIgnored weak var sidebarScroll: NSScrollView?
    @ObservationIgnored var sidebarScrollBy: (CGFloat) -> Void = { _ in }
    @ObservationIgnored private var timer: Timer?
    /// When the button was first seen up during a drag, for ending a drag whose session never said so.
    @ObservationIgnored private var buttonUpSince: Date?
    /// How long the button may be up before such a drag counts as lost. The drop arrives a moment after the release
    /// and needs the drag to still be on.
    static let lostSessionAfter: TimeInterval = 0.5
    #if DEBUG
    /// For CoreCheck, a pointer in window coordinates that stands in for the real one and the held button.
    @ObservationIgnored var debugPointer: NSPoint?
    /// For CoreCheck, each drop target's frame in the window, from the top left.
    @ObservationIgnored var debugFrames: [String: CGRect] = [:]
    #endif

    func begin(_ songs: [SongRow], fromQueue: Bool, targets: Set<String>) {
        guard !songs.isEmpty else { return }
        self.songs = songs
        self.fromQueue = fromQueue
        self.targets = targets
        over = nil
        refused = false
        active = true
        buttonUpSince = nil
        timer?.invalidate()
        let timer = Timer(timeInterval: 1.0 / 60, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.tick() }
        }
        // Drags run the event loop in its tracking mode.
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
    }

    func end() {
        guard active else { return }
        active = false
        over = nil
        refused = false
        timer?.invalidate()
        timer = nil
    }

    func takes(_ target: SongDropTarget) -> Bool {
        switch target {
        case .playlist(let p, _): targets.contains(p.id)
        case .queueEnd, .queue: !fromQueue
        }
    }

    /// The pointer came over a target. `taking` overrides whether it takes the songs.
    func enter(_ target: SongDropTarget, taking: Bool? = nil) {
        guard active else { return }
        if taking ?? takes(target) { over = target.key; refused = false } else { over = nil; refused = true }
    }

    func exit(_ target: SongDropTarget) {
        if over == target.key { over = nil }
        refused = false
    }

    /// Runs every frame of a drag. Scrolls the sidebar near its edges, shows the not-allowed cursor over a refused
    /// target, and ends a drag whose session ended without telling us.
    private func tick() {
        guard active else { return }
        var pointer: NSPoint?
        #if DEBUG
        // CoreCheck's drags have no real pointer or button.
        pointer = debugPointer
        #endif
        if pointer == nil, NSEvent.pressedMouseButtons & 1 == 0 {
            let since = buttonUpSince ?? Date()
            buttonUpSince = since
            if Date().timeIntervalSince(since) >= Self.lostSessionAfter { end() }
            return
        }
        buttonUpSince = nil
        if refused { NSCursor.operationNotAllowed.set() }
        if let scroll = sidebarScroll, let window = scroll.window {
            let point = pointer ?? window.convertPoint(fromScreen: NSEvent.mouseLocation)
            let box = scroll.convert(scroll.bounds, to: nil)
            guard point.x >= box.minX, point.x <= box.maxX, point.y >= box.minY, point.y <= box.maxY else { return }
            let speed = EdgeScroll.speed(top: box.maxY - point.y, bottom: point.y - box.minY)
            if speed != 0 { sidebarScrollBy(speed) }
        }
    }
}

@MainActor enum EdgeScroll {
    /// Distance from the visible edge where scrolling starts, and the top speed per tick at 60 ticks a second.
    static let edge: CGFloat = 56
    static let speed: CGFloat = 18

    /// The speed for a pointer `top` from the visible top and `bottom` from the bottom. Up is negative.
    static func speed(top: CGFloat, bottom: CGFloat) -> CGFloat {
        top < edge ? -speed * (1 - max(0, top) / edge) : bottom < edge ? speed * (1 - max(0, bottom) / edge) : 0
    }

    /// Scrolls by `speed`, down positive, within the content. True when it moved.
    @discardableResult static func scroll(_ scroll: NSScrollView, by speed: CGFloat) -> Bool {
        guard speed != 0 else { return false }
        let clip = scroll.contentView
        let docHeight = scroll.documentView?.frame.height ?? 0
        var origin = clip.bounds.origin
        let limit = max(0, docHeight - clip.bounds.height)
        origin.y = min(limit, max(0, origin.y + (clip.isFlipped ? speed : -speed)))
        guard origin.y != clip.bounds.origin.y else { return false }
        clip.scroll(to: origin)
        scroll.reflectScrolledClipView(clip)
        return true
    }
}

/// Finds the scroll view this sits in and hands it over.
struct ScrollViewProbe: NSViewRepresentable {
    let found: (NSScrollView) -> Void

    func makeNSView(context: Context) -> ProbeView { ProbeView() }
    func updateNSView(_ view: ProbeView, context: Context) {
        let found = found
        DispatchQueue.main.async { if let scroll = view.enclosingScrollView { found(scroll) } }
    }

    final class ProbeView: NSView {
        override func hitTest(_ point: NSPoint) -> NSView? { nil }
    }
}

extension ShellModel {
    /// Starts carrying songs out of a list, reading which playlists take them.
    func beginSongDrag(_ songs: [SongRow], fromQueue: Bool = false) {
        songDrag.begin(songs, fromQueue: fromQueue, targets: Set(library.playlistTargets().map(\.id)))
    }

    func dropSongs(on target: SongDropTarget) {
        let songs = songDrag.songs
        songDrag.end()
        guard !songs.isEmpty else { return }
        switch target {
        case .playlist(let p, _): addSongs(songs, toPlaylist: p)
        case .queueEnd: queueSongsAtEnd(songs)
        case .queue: break
        }
    }
}

/// A playlist, the queue tab or the bar's queue button as a drop target. A target that takes the songs fills
/// with `dropTarget` and an accent ring while they're over it.
struct SongDropDelegate: DropDelegate {
    let target: SongDropTarget
    let shell: ShellModel

    func validateDrop(info: DropInfo) -> Bool { shell.songDrag.active && info.hasItemsConforming(to: [.naviaSongs]) }
    func dropEntered(info: DropInfo) { shell.songDrag.enter(target) }
    func dropUpdated(info: DropInfo) -> DropProposal? { DropProposal(operation: shell.songDrag.takes(target) ? .copy : .forbidden) }
    func dropExited(info: DropInfo) { shell.songDrag.exit(target) }
    func performDrop(info: DropInfo) -> Bool {
        shell.songDrag.exit(target)
        guard shell.songDrag.takes(target) else { return false }
        shell.dropSongs(on: target)
        return true
    }
}

extension View {
    /// Makes this a drop target for dragged songs, with its highlight spread `pad` past the view.
    func songDropTarget(_ target: SongDropTarget, shell: ShellModel, pad: CGFloat = 0) -> some View {
        modifier(SongDropHighlight(target: target, shell: shell, pad: pad))
    }
}

private struct SongDropHighlight: ViewModifier {
    let target: SongDropTarget
    let shell: ShellModel
    let pad: CGFloat

    func body(content: Content) -> some View {
        let on = shell.songDrag.over == target.key
        let shape = RoundedRectangle(cornerRadius: T.rMD + pad, style: .continuous)
        content
            .background {
                if on { shape.fill(shell.theme.dropTarget.color).padding(-pad) }
            }
            .overlay {
                if on { shape.strokeBorder(shell.theme.dropTargetEdge.color, lineWidth: T.dropRing).padding(-pad).allowsHitTesting(false) }
            }
            .onDrop(of: [.naviaSongs], delegate: SongDropDelegate(target: target, shell: shell))
            .debugDropFrame(target.key, shell: shell)
    }
}

extension View {
    /// The view's frame in the window, so CoreCheck can find drop targets by key. Does nothing in release builds.
    func debugDropFrame(_ key: String, shell: ShellModel) -> some View {
        #if DEBUG
        onGeometryChange(for: CGRect.self, of: { $0.frame(in: .global) }) { [songDrag = shell.songDrag] in songDrag.debugFrames[key] = $0 }
        #else
        self
        #endif
    }
}

/// The card that follows the pointer while songs are dragged. It shows the lead song's cover, title and artist,
/// and for several songs a second card underneath with the count.
struct SongDragCard: View {
    let lead: SongRow
    let count: Int
    /// already decoded, or nil for the placeholder.
    let art: NSImage?
    let accent: RGBA
    let onAccent: RGBA

    /// in the rendered image.
    static var shadowRoom: CGFloat { T.px(28) }

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: T.rMD, style: .continuous)
        let thumb = RoundedRectangle(cornerRadius: T.rSM, style: .continuous)
        let height = T.thumb + T.px(16)
        HStack(spacing: T.px(12)) {
            Group {
                if let art {
                    Image(nsImage: art).resizable().aspectRatio(contentMode: .fill)
                } else {
                    T.dragCardUnder.color
                }
            }
            .frame(width: T.thumb, height: T.thumb)
            .clipShape(thumb)
            VStack(alignment: .leading, spacing: 0) {
                Text(lead.title).font(.navia(T.textMD, .medium)).foregroundStyle(T.t1.color).lineLimit(1)
                Text(lead.artist).font(.navia(T.textSM)).foregroundStyle(T.t2.color).lineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.leading, T.px(8))
        .padding(.trailing, T.px(12))
        .frame(width: T.dragOutCardW, height: height)
        .background(shape.fill(T.dragCard.color))
        .overlay(shape.strokeBorder(T.dragCardEdge.color, lineWidth: 1))
        .background {
            if count > 1 {
                shape.fill(T.dragCardUnder.color)
                    .overlay(shape.strokeBorder(T.dragCardUnderEdge.color, lineWidth: 1))
                    .offset(y: T.px(6))
            }
        }
        .shadow(T.shadowLift)
        .overlay(alignment: .topTrailing) {
            if count > 1 {
                CountBadge(text: Ln("{count} song", "{count} songs", count), accent: accent, onAccent: onAccent)
                    .offset(x: T.px(8), y: -T.px(10))
            }
        }
        .padding(Self.shadowRoom)
        .environment(\.colorScheme, .dark)
    }

    /// The card as a drag image, below and right of `point` in `view`. The cover comes from the shared artwork
    /// cache, so it shows at once.
    @MainActor static func dragImage(lead: SongRow, count: Int, at point: NSPoint, in view: NSView, theme: Theme) -> (image: NSImage, frame: NSRect)? {
        let pixels = Int(T.thumb * 2)
        let art = lead.artworkURL.flatMap { ArtworkLoader.shared.cached($0, pixels: pixels) ?? ArtworkLoader.shared.loadNow($0, pixels: pixels) }
        let renderer = ImageRenderer(content: SongDragCard(lead: lead, count: count, art: art, accent: theme.accent, onAccent: theme.onAccent))
        renderer.scale = view.window?.backingScaleFactor ?? 2
        guard let image = renderer.nsImage else { return nil }
        let room = shadowRoom
        let x = point.x + T.dragOutOffsetX - room
        let y = view.isFlipped ? point.y + T.dragOutOffsetY - room : point.y - T.dragOutOffsetY - image.size.height + room
        return (image, NSRect(x: x, y: y, width: image.size.width, height: image.size.height))
    }
}

/// Starts an AppKit drag of songs from a SwiftUI list, such as the queue once the pointer leaves it.
@MainActor final class SongDragSource: NSObject, NSDraggingSource {
    static let shared = SongDragSource()
    private weak var shell: ShellModel?

    func begin(_ songs: [SongRow], fromQueue: Bool, event: NSEvent, in view: NSView, shell: ShellModel) {
        guard let lead = songs.first else { return }
        self.shell = shell
        let item = NSPasteboardItem()
        item.setString(songs.map(\.id).joined(separator: "\n"), forType: .naviaSongs)
        let dragging = NSDraggingItem(pasteboardWriter: item)
        let point = view.convert(event.locationInWindow, from: nil)
        if let card = SongDragCard.dragImage(lead: lead, count: songs.count, at: point, in: view, theme: shell.theme) {
            dragging.setDraggingFrame(card.frame, contents: card.image)
        }
        shell.beginSongDrag(songs, fromQueue: fromQueue)
        let session = view.beginDraggingSession(with: [dragging], event: event, source: self)
        session.animatesToStartingPositionsOnCancelOrFail = false
        session.draggingFormation = .none
    }

    func draggingSession(_ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext) -> NSDragOperation {
        context == .withinApplication ? .copy : []
    }

    func draggingSession(_ session: NSDraggingSession, endedAt screenPoint: NSPoint, operation: NSDragOperation) {
        shell?.songDrag.end()
    }
}
