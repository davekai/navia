import AppKit
import SwiftUI

/// The mini player's sizes. It resizes freely between `T.miniMinW` x `T.miniMinH` and `T.miniMax` square, and
/// stacks when tall enough. Sizes are whole points, since AppKit rounds window sizes and would nudge a
/// fractional one on every placement.
enum MiniSize {
    static var compact: NSSize { NSSize(width: T.miniW.rounded(), height: T.miniH.rounded()) }
    static var large: NSSize { NSSize(width: T.miniLarge.rounded(), height: T.miniLarge.rounded()) }
    static var min: NSSize { NSSize(width: T.miniMinW.rounded(), height: T.miniMinH.rounded()) }
    static var max: NSSize { NSSize(width: T.miniMax.rounded(), height: T.miniMax.rounded()) }

    static func stacked(_ size: CGSize) -> Bool { size.height >= T.miniStackH && size.height >= size.width * T.miniStackRatio }

    static func clamped(_ size: NSSize) -> NSSize {
        NSSize(width: Swift.min(Swift.max(size.width, min.width), max.width).rounded(),
               height: Swift.min(Swift.max(size.height, min.height), max.height).rounded())
    }
}

/// The mini player, a small floating panel above every window and on every Space. Opening it hides the main window,
/// and the back button, a link, the Dock icon, the menu item or ⌥⌘M bring the window back.
/// Its position is remembered per screen.
@MainActor @Observable final class MiniPlayer {
    static let shared = MiniPlayer()

    static let symbol = "pip"
    static let shortcutKey = "m"
    static let shortcutModifiers: NSEvent.ModifierFlags = [.command, .option]
    static let shortcutLabel = "⌥⌘M"
    private static let keepAlive = "The mini player is open"

    /// ⌥⌘M, read by key position, since Option+M types a character.
    static func isShortcut(_ event: NSEvent) -> Bool {
        event.keyCode == 46 && event.modifierFlags.intersection([.command, .option, .control, .shift]) == shortcutModifiers
    }

    private(set) var isOpen = false
    /// The size it was last left at, shared by all screens.
    private(set) var size: NSSize
    /// The pointer is over the panel. The panel tracks this itself, so it works while Navia isn't active.
    var hovering = false

    @ObservationIgnored weak var mainWindow: NSWindow?
    @ObservationIgnored private(set) var panel: MiniPanel?
    @ObservationIgnored private weak var shell: ShellModel?
    @ObservationIgnored private var observers: [NSObjectProtocol] = []

    private init() {
        size = MiniSize.clamped(UserDefaults.standard.string(forKey: "miniPlayer.contentSize").map(NSSizeFromString) ?? MiniSize.compact)
    }

    func toggle(shell: ShellModel) {
        if isOpen { close() } else { open(shell: shell) }
    }

    /// Opened because the main window was minimized. The window stays in the Dock, and restoring it closes this.
    private(set) var openedForMinimize = false

    /// `forMinimize` means the main window was just minimized, so it stays in the Dock instead of being ordered out.
    func open(shell: ShellModel, forMinimize: Bool = false) {
        guard !isOpen, shell.player.current != nil else { return }
        openedForMinimize = forMinimize
        self.shell = shell
        let panel = self.panel ?? makePanel(shell: shell)
        self.panel = panel
        let screen = mainWindow?.screen ?? NSScreen.main ?? NSScreen.screens.first
        panel.setFrame(placement(on: screen), display: false)
        isOpen = true
        hovering = false
        // With the main window ordered out, macOS would count Navia as windowless and could quietly terminate it.
        ProcessInfo.processInfo.disableAutomaticTermination(Self.keepAlive)
        shell.setMiniPlayerShown(true)
        panel.orderFrontRegardless()
        if !forMinimize { mainWindow?.orderOut(nil) }
    }

    /// Returns to the full window. Links call `ShellModel.go`, which ends up here too.
    func close() {
        guard isOpen else { return }
        save()
        isOpen = false
        openedForMinimize = false
        panel?.orderOut(nil)
        ProcessInfo.processInfo.enableAutomaticTermination(Self.keepAlive)
        shell?.setMiniPlayerShown(false)
        mainWindow?.makeKeyAndOrderFront(nil)
        NSApp.activate()
    }

    /// Toggles between compact and the large square, keeping the nearest corner in place.
    func switchSize() { resize(to: MiniSize.stacked(size) ? MiniSize.compact : MiniSize.large) }

    /// Resizes within the same limits as dragging the edges, keeping the nearest corner in place.
    func resize(to next: NSSize) {
        guard let panel, let area = panel.screen?.visibleFrame else { return }
        panel.setFrame(Self.resized(panel.frame, to: MiniSize.clamped(next), in: area), display: true)
    }

    private func sizeChanged() {
        guard let panel else { return }
        let now = panel.contentLayoutRect.size
        if now != size { size = now }
        UserDefaults.standard.set(NSStringFromSize(now), forKey: "miniPlayer.contentSize")
        panel.invalidateShadow()
        save()
    }

    private func makePanel(shell: ShellModel) -> MiniPanel {
        let panel = MiniPanel(contentRect: NSRect(origin: .zero, size: size), styleMask: [.borderless, .nonactivatingPanel, .resizable], backing: .buffered, defer: false)
        panel.isFloatingPanel = true
        panel.level = .floating
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.isOpaque = false
        panel.backgroundColor = .clear
        // The system's window shadow follows the rounded shape. A SwiftUI shadow would be cut at the panel's edge.
        panel.hasShadow = true
        panel.animationBehavior = .utilityWindow
        let host = MiniHostingView(rootView: MiniPlayerView(shell: shell, mini: self))
        host.sizingOptions = []
        host.onHover = { [weak self] in self?.hovering = $0 }
        // Blurs what's behind the panel, cut to the card's corners. SwiftUI's glass would distort the card's edges.
        let blur = NSVisualEffectView(frame: NSRect(origin: .zero, size: size))
        blur.material = T.windowMaterial
        blur.blendingMode = .behindWindow
        blur.state = .active
        blur.maskImage = Self.cornerMask(T.miniR)
        host.frame = blur.bounds
        host.autoresizingMask = [.width, .height]
        blur.addSubview(host)
        // Over the card, so its drag gesture and controls never take the edges.
        let grips = MiniResizeGrips(frame: blur.bounds)
        grips.autoresizingMask = [.width, .height]
        blur.addSubview(grips)
        panel.contentView = blur
        // Set after the content view, which would otherwise reset them.
        panel.contentMinSize = MiniSize.min
        panel.contentMaxSize = MiniSize.max
        let center = NotificationCenter.default
        observers = [
            center.addObserver(forName: NSWindow.didMoveNotification, object: panel, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.save() }
            },
            center.addObserver(forName: NSWindow.didResizeNotification, object: panel, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.sizeChanged() }
            },
        ]
        return panel
    }

    private static func cornerMask(_ radius: CGFloat) -> NSImage {
        let side = radius * 2 + 1
        let image = NSImage(size: NSSize(width: side, height: side), flipped: false) { rect in
            NSColor.black.setFill()
            NSBezierPath(roundedRect: rect, xRadius: radius, yRadius: radius).fill()
            return true
        }
        image.capInsets = NSEdgeInsets(top: radius, left: radius, bottom: radius, right: radius)
        image.resizingMode = .stretch
        return image
    }

    // MARK: Place

    private static func key(_ screen: NSScreen) -> String {
        let id = (screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value ?? 0
        return "miniPlayer.frame.\(id)"
    }

    private func save() {
        guard isOpen, let panel, let screen = panel.screen else { return }
        UserDefaults.standard.set(NSStringFromRect(panel.frame), forKey: Self.key(screen))
    }

    /// Where it last sat on this screen, at the current size and kept on screen. Defaults to the bottom right.
    private func placement(on screen: NSScreen?) -> NSRect {
        let side = size
        guard let screen else { return NSRect(origin: .zero, size: side) }
        let area = screen.visibleFrame
        if let saved = UserDefaults.standard.string(forKey: Self.key(screen)) {
            return Self.resized(NSRectFromString(saved), to: side, in: area)
        }
        let margin = T.miniCornerMargin
        return NSRect(x: area.maxX - side.width - margin, y: area.minY + margin, width: side.width, height: side.height)
    }

    /// `frame` at `size`, keeping its nearest corner of `area` in place, then kept inside `area`.
    static func resized(_ frame: NSRect, to size: NSSize, in area: NSRect) -> NSRect {
        let right = frame.midX > area.midX, top = frame.midY > area.midY
        var out = NSRect(x: right ? frame.maxX - size.width : frame.minX, y: top ? frame.maxY - size.height : frame.minY,
                         width: size.width, height: size.height)
        out.origin.x = min(max(out.minX, area.minX), area.maxX - size.width)
        out.origin.y = min(max(out.minY, area.minY), area.maxY - size.height)
        return out
    }
}

/// The resize grips, a band along each edge with larger squares at the corners. They sit over the card so its
/// drag gesture and controls can't swallow them. A drag resizes from that edge, keeping the opposite side in place.
final class MiniResizeGrips: NSView {
    private struct Edges: OptionSet {
        let rawValue: Int
        static let left = Edges(rawValue: 1), right = Edges(rawValue: 2), top = Edges(rawValue: 4), bottom = Edges(rawValue: 8)
    }
    private var drag: (edges: Edges, start: NSPoint, frame: NSRect)?

    /// Which edges a point resizes, in view coordinates with y up.
    private func edges(at p: NSPoint) -> Edges {
        let g = T.miniGrip, corner = g * 2
        let near = (left: p.x < corner, right: p.x > bounds.width - corner, bottom: p.y < corner, top: p.y > bounds.height - corner)
        var out: Edges = []
        if (near.left || near.right) && (near.top || near.bottom) {
            out.insert(near.left ? .left : .right)
            out.insert(near.top ? .top : .bottom)
            return out
        }
        if p.x < g { out.insert(.left) } else if p.x > bounds.width - g { out.insert(.right) }
        if p.y < g { out.insert(.bottom) } else if p.y > bounds.height - g { out.insert(.top) }
        return out
    }

    override func hitTest(_ point: NSPoint) -> NSView? {
        edges(at: convert(point, from: superview)).isEmpty ? nil : self
    }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func resetCursorRects() {
        let g = T.miniGrip, corner = g * 2, w = bounds.width, h = bounds.height
        let cursor = { (position: NSCursor.FrameResizePosition) in NSCursor.frameResize(position: position, directions: .all) }
        addCursorRect(NSRect(x: corner, y: h - g, width: w - corner * 2, height: g), cursor: cursor(.top))
        addCursorRect(NSRect(x: corner, y: 0, width: w - corner * 2, height: g), cursor: cursor(.bottom))
        addCursorRect(NSRect(x: 0, y: corner, width: g, height: h - corner * 2), cursor: cursor(.left))
        addCursorRect(NSRect(x: w - g, y: corner, width: g, height: h - corner * 2), cursor: cursor(.right))
        addCursorRect(NSRect(x: 0, y: h - corner, width: corner, height: corner), cursor: cursor(.topLeft))
        addCursorRect(NSRect(x: w - corner, y: h - corner, width: corner, height: corner), cursor: cursor(.topRight))
        addCursorRect(NSRect(x: 0, y: 0, width: corner, height: corner), cursor: cursor(.bottomLeft))
        addCursorRect(NSRect(x: w - corner, y: 0, width: corner, height: corner), cursor: cursor(.bottomRight))
    }

    override func mouseDown(with event: NSEvent) {
        guard let window else { return }
        drag = (edges(at: convert(event.locationInWindow, from: nil)), window.convertPoint(toScreen: event.locationInWindow), window.frame)
    }

    override func mouseDragged(with event: NSEvent) {
        guard let drag, let window else { return }
        let now = window.convertPoint(toScreen: event.locationInWindow)
        let dx = now.x - drag.start.x, dy = now.y - drag.start.y
        let f = drag.frame
        var size = f.size
        if drag.edges.contains(.left) { size.width -= dx } else if drag.edges.contains(.right) { size.width += dx }
        if drag.edges.contains(.bottom) { size.height -= dy } else if drag.edges.contains(.top) { size.height += dy }
        size = MiniSize.clamped(size)
        let x = drag.edges.contains(.left) ? f.maxX - size.width : f.minX
        let y = drag.edges.contains(.bottom) ? f.maxY - size.height : f.minY
        window.setFrame(NSRect(x: x, y: y, width: size.width, height: size.height), display: true)
    }

    override func mouseUp(with event: NSEvent) { drag = nil }

    override func layout() {
        super.layout()
        window?.invalidateCursorRects(for: self)
    }
}

/// A borderless panel that can take the keyboard without making Navia the active app.
final class MiniPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}

/// Takes the first click while Navia isn't active, and tracks the pointer either way.
final class MiniHostingView<Content: View>: NSHostingView<Content> {
    var onHover: (Bool) -> Void = { _ in }
    private var area: NSTrackingArea?

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let area { removeTrackingArea(area) }
        let next = NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self, userInfo: nil)
        addTrackingArea(next)
        area = next
    }

    override func mouseEntered(with event: NSEvent) {
        super.mouseEntered(with: event)
        onHover(true)
    }

    override func mouseExited(with event: NSEvent) {
        super.mouseExited(with: event)
        onHover(false)
    }
}

struct MiniPlayerView: View {
    let shell: ShellModel
    let mini: MiniPlayer

    var body: some View {
        GeometryReader { geo in MiniCard(shell: shell, mini: mini, side: geo.size) }
    }
}

/// The compact layout puts a square cover beside the title, artist, lyric line, transport and seek bar. The stacked
/// layout fills the card with the cover and centers the rest over a scrim. The size and back buttons show on hover.
private struct MiniCard: View {
    let shell: ShellModel
    let mini: MiniPlayer
    let side: CGSize
    private var player: any PlayerViewModel { shell.player }
    private var large: Bool { MiniSize.stacked(side) }

    var body: some View {
        let tint = Tint(hex: player.current?.tintHex)
        let shade = tint.playerVivid.mixed(with: T.dockShade, amount: T.playerShadeMix)
        let shape = RoundedRectangle(cornerRadius: T.miniR, style: .continuous)
        ZStack(alignment: .topTrailing) {
            // Every empty spot drags the panel. The controls on top take their own clicks.
            Color.clear.contentShape(Rectangle()).gesture(WindowDragGesture())
            if large {
                cover
                LinearGradient(stops: [.init(color: .clear, location: T.miniScrimStart),
                                       .init(color: shade.opacity(T.miniScrimStrength).color, location: 1)], startPoint: .top, endPoint: .bottom)
                    .allowsHitTesting(false)
                largeBody.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottom)
            } else {
                HStack(spacing: 0) {
                    cover.frame(width: side.height, height: side.height)
                    compactBody
                }
            }
            chrome
        }
        .frame(width: side.width, height: side.height)
        .foregroundStyle(tint.playerFg.color)
        .environment(\.playerDim, tint.playerFgDim.color)
        .environment(\.hoverInk, tint.playerFg.color)
        .background(LinearGradient(colors: [tint.playerVivid.opacity(T.miniGlass).color, shade.opacity(T.miniGlass).color], startPoint: .top, endPoint: .bottom))
        .clipShape(shape)
        .overlay(shape.strokeBorder(T.glassEdge.color, lineWidth: 1).allowsHitTesting(false))
        .accessibilityElement(children: .contain)
        .accessibilityLabel(L("Mini Player"))
    }

    private var cover: some View {
        ArtworkView(url: player.current?.artworkURL, kind: player.station == nil ? .album : .station, pixels: large ? 700 : 240)
            .gesture(WindowDragGesture())
    }

    private var lyricLine: String? {
        let lines = player.lyrics
        guard player.station == nil, player.lyricsAreSynced(lines) else { return nil }
        let text = lines[lines.activeIndex(atMs: Int(player.position * 1000))].text
        return text.isEmpty ? nil : text
    }

    private func info(center: Bool) -> some View {
        VStack(alignment: center ? .center : .leading, spacing: 0) {
            if let song = player.current {
                let titleSize = center ? T.textLG : T.textMD
                MarqueeText {
                    HStack(spacing: titleSize * 0.27) {
                        LinkedText(pieces: player.station == nil ? [LinkPiece.title(song)] : LinkPiece.plain(song.title), shell: shell)
                            .font(.navia(titleSize, .medium)).foregroundStyle(T.tOnArt.color)
                        if song.isExplicit { ExplicitMark(size: titleSize) }
                    }
                }
                MarqueeText {
                    LinkedText(pieces: player.station == nil ? LinkPiece.artist(song) : LinkPiece.plain(song.artist), shell: shell)
                        .font(.navia(T.textSM)).foregroundStyle(tint.playerFgDim.color)
                }
                if let lyricLine {
                    Text(lyricLine).font(.navia(T.textXS)).foregroundStyle(tint.playerFgDim.color)
                }
            }
        }
        .lineLimit(1)
        // Right-click on the song opens the player's menu, as in the bar and the dock.
        .rightClickMenu { playerMenu(shell) }
    }

    private var tint: Tint { Tint(hex: player.current?.tintHex) }

    private func transport(skip: CGFloat, play: CGFloat, box: CGFloat?) -> some View {
        HStack(spacing: box == nil ? T.miniGap * 2 : 0) {
            SkipButton(symbol: "backward.fill", size: skip, label: L("Previous"), disabled: player.station != nil, box: box) { player.previous() }
            PlayPauseButton(player: player, size: play, box: box)
            SkipButton(symbol: "forward.fill", size: skip, label: L("Next"), disabled: player.station != nil, box: box) { player.next() }
        }
    }

    @ViewBuilder private var seek: some View {
        if player.station != nil {
            Capsule().fill(.foreground).opacity(0.3).frame(height: T.scrubThin).frame(height: T.px(14))
        } else {
            let total = Double(player.current?.duration ?? 0)
            TrackSlider(value: player.position, total: total, rest: T.playerTrack, thin: true,
                        chip: "\(clock(player.position)) / \(ScrubTime.endText(shell))") { player.seek(to: $0) }
        }
    }

    private var compactBody: some View {
        VStack(spacing: 0) {
            HStack(alignment: .bottom, spacing: T.miniGap) {
                info(center: false).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
                transport(skip: T.px(20), play: T.px(28), box: T.miniButton)
            }
            .frame(maxHeight: .infinity)
            seek
        }
        .padding(.top, T.miniPad)
        .padding(.trailing, T.miniPad)
        .padding(.bottom, T.miniPad / 2)
        .padding(.leading, T.miniGap)
    }

    private var largeBody: some View {
        VStack(spacing: T.miniPad / 2) {
            info(center: true)
            seek
            transport(skip: T.px(26), play: T.px(38), box: nil)
        }
        .padding(.horizontal, T.miniPad * 1.5)
        .padding(.bottom, T.miniPad)
    }

    private var chrome: some View {
        HStack(spacing: large ? T.miniGap / 2 : T.px(2)) {
            let button = large ? T.controlH : T.miniChromeButton
            IconButton(symbol: large ? "arrow.down.right.and.arrow.up.left" : "arrow.up.left.and.arrow.down.right",
                       label: large ? L("Smaller") : L("Larger"), size: T.px(large ? 17 : 15), weight: .bold,
                       width: button, height: button, onArt: large) { mini.switchSize() }
            IconButton(symbol: "macwindow", label: L("Back to Full Window"), size: T.px(large ? 19 : 17),
                       width: button, height: button, onArt: large) { mini.close() }
        }
        .foregroundStyle(large ? T.tOnArt.color : tint.playerFg.color)
        .padding(.top, large ? T.miniPad : T.miniPad / 3)
        .padding(.trailing, large ? T.miniPad : T.miniPad / 3)
        .opacity(mini.hovering || HoverDebug.forced("MiniChrome") ? 1 : 0)
        .animation(T.reveal(mini.hovering), value: mini.hovering)
    }
}
