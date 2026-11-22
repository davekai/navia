import AppKit
import NaviaCoreFFI
import SwiftUI

@main
struct NaviaApp: App {
    @State private var shell: ShellModel
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate

    init() {
        BundleMove.carrySettings() // Before anything reads a setting.
        startLogging(subsystem: Log.subsystem) // before the shell starts the core.
        _shell = State(initialValue: ShellModel())
        NSApplication.shared.appearance = NSAppearance(named: .darkAqua)
        _ = Motion.shared // before anything animates.
    }

    var body: some Scene {
        Window("Navia", id: "main") {
            RootView(shell: shell)
                .frame(minWidth: 620, minHeight: 480)
                #if DEBUG
                .background(DiagnosticsRunner(shell: shell))
                #endif
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 1400, height: 875)
        .commands { NaviaCommands(shell: shell) }
    }
}

/// Clicking the Dock icon while the mini player is up brings the main window back.
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        guard MiniPlayer.shared.isOpen else { return true }
        MiniPlayer.shared.close()
        return false
    }

    /// Closing the window quits Navia, but not while the mini player hides it, since AppKit counts an ordered-out
    /// window as closed.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { !MiniPlayer.shared.isOpen }
}

/// Handles a double-click on the top bar's empty space or the sidebar header the way the system's title bar
/// setting asks, since the content covers the title bar.
/// Two paths catch it, so it acts exactly once. Where the view under the click lets the window move, an event
/// monitor acts before the window sees it, since AppKit ignores real double-clicks there. Elsewhere this
/// responder sits between the content view and the window frame and only gets clicks no view took.
@MainActor final class TitlebarDoubleClick: NSResponder {
    private weak var window: NSWindow?
    private static var installed: [ObjectIdentifier: TitlebarDoubleClick] = [:]

    private static var monitor: Any?

    /// Puts the responder in place, again whenever AppKit rebuilds the chain.
    static func install(in window: NSWindow) {
        if monitor == nil {
            monitor = NSEvent.addLocalMonitorForEvents(matching: .leftMouseDown) { event in
                let handled = MainActor.assumeIsolated { () -> Bool in
                    guard event.clickCount == 2, let window = event.window, installed[ObjectIdentifier(window)] != nil,
                          inTopBar(event, window), appKitZooms(event, window) else { return false }
                    perform(on: window)
                    return true
                }
                return handled ? nil : event
            }
        }
        guard let content = window.contentView, let frame = content.superview else { return }
        let key = ObjectIdentifier(window)
        let responder = installed[key] ?? TitlebarDoubleClick()
        installed[key] = responder
        responder.window = window
        guard content.nextResponder !== responder else { return }
        responder.nextResponder = frame
        content.nextResponder = responder
    }

    override func mouseDown(with event: NSEvent) {
        if event.clickCount == 2, let window, Self.inTopBar(event, window), !Self.appKitZooms(event, window) {
            Self.perform(on: window)
            return
        }
        nextResponder?.mouseDown(with: event)
    }

    /// Whether the view under the click lets the window move, which is where the monitor acts.
    /// The window's own SwiftUI view answers yes for every pill in the top bar, so it's excluded and its clicks
    /// reach the responder only when nothing took them. Song rows scrolled under the bar count as empty space.
    private static func appKitZooms(_ event: NSEvent, _ window: NSWindow) -> Bool {
        guard let content = window.contentView, let root = content.superview,
              let hit = root.hitTest(root.convert(event.locationInWindow, from: nil)), hit !== content else { return false }
        if hit is SongsNSTableView || hit is SongCell || (hit.superview is SongCell && !(hit is NSButton)) { return true }
        return hit.mouseDownCanMoveWindow
    }

    /// A double-click a top bar view took but that belongs to the title bar, such as on a song row scrolled under it.
    /// True when it was handled here.
    static func handleInTopBar(_ event: NSEvent) -> Bool {
        guard event.clickCount == 2, let window = event.window, inTopBar(event, window) else { return false }
        perform(on: window)
        return true
    }

    /// A row scrolled under the top bar: pressing there moves the window instead of picking up the row.
    static func movesWindow(_ event: NSEvent) -> Bool {
        guard event.clickCount == 1, let window = event.window, inTopBar(event, window) else { return false }
        window.performDrag(with: event)
        return true
    }

    /// also holds the sidebar header.
    static func inTopBar(_ event: NSEvent, _ window: NSWindow) -> Bool {
        guard let content = window.contentView, !window.styleMask.contains(.fullScreen) else { return false }
        return content.bounds.height - event.locationInWindow.y <= T.topbarH
    }

    /// The system setting: `Maximize` (the default), `Minimize` or `None`. Older systems only have
    /// `AppleMiniaturizeOnDoubleClick`. `Fill` has no public call, so it zooms too.
    static var action: String {
        let defaults = UserDefaults.standard
        if let action = defaults.string(forKey: "AppleActionOnDoubleClick") { return action }
        return defaults.bool(forKey: "AppleMiniaturizeOnDoubleClick") ? "Minimize" : "Maximize"
    }

    #if DEBUG
    /// The last action taken, for CoreCheck. `-naviaDoubleClickDry YES` only records it.
    static var last = ""
    static var performed = 0
    static var dry: Bool { UserDefaults.standard.bool(forKey: "naviaDoubleClickDry") }
    #endif

    private static func perform(on window: NSWindow) {
        let action = action
        #if DEBUG
        last = action
        if dry { return }
        performed += 1
        #endif
        switch action {
        case "None": break
        case "Minimize": window.miniaturize(nil)
        default: window.zoom(nil)
        }
    }
}

struct WindowConfigurator: NSViewRepresentable {
    let translucent: Bool
    let shell: ShellModel

    func makeNSView(context: Context) -> NSView {
        let view = ConfiguringView()
        view.shell = shell
        view.translucent = translucent
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        guard let view = nsView as? ConfiguringView else { return }
        view.translucent = translucent
        view.applyBackground()
    }

    private final class ConfiguringView: NSView {
        private var observers: [NSObjectProtocol] = []
        var shell: ShellModel?
        var translucent = false

        isolated deinit { observers.forEach(NotificationCenter.default.removeObserver) }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            guard let window else { return }
            MiniPlayer.shared.mainWindow = window
            window.titleVisibility = .hidden
            window.titlebarAppearsTransparent = true
            window.styleMask.insert(.fullSizeContentView)
            applyBackground()
            // An empty unified toolbar gives the taller title area, so the window buttons sit lower and the corners are larger.
            if window.toolbar == nil {
                let toolbar = NSToolbar(identifier: "main")
                toolbar.showsBaselineSeparator = false
                window.toolbar = toolbar
                window.toolbarStyle = .unified
            }
            placeButtons(in: window)
            TitlebarDoubleClick.install(in: window)
            // AppKit focuses a window's first text field on open, which draws its focus ring and takes Space from play and pause.
            DispatchQueue.main.async { [weak window] in
                guard let window, window.firstResponder is NSText else { return }
                window.makeFirstResponder(nil)
            }
            guard observers.isEmpty else { return }
            let center = NotificationCenter.default
            for name in [NSWindow.didResizeNotification, NSWindow.didBecomeKeyNotification, NSWindow.didResignKeyNotification,
                         NSWindow.willEnterFullScreenNotification, NSWindow.didEnterFullScreenNotification, NSWindow.didExitFullScreenNotification] {
                observers.append(center.addObserver(forName: name, object: window, queue: .main) { [weak self, weak window] note in
                    let entering = note.name == NSWindow.willEnterFullScreenNotification
                    MainActor.assumeIsolated {
                        guard let self, let window else { return }
                        self.placeButtons(in: window)
                        TitlebarDoubleClick.install(in: window)
                        // The style mask isn't updated yet when entering starts, and going solid first keeps the transition clean.
                        let full = entering || window.styleMask.contains(.fullScreen)
                        if self.shell?.inFullScreen != full { self.shell?.inFullScreen = full }
                    }
                })
            }
            // while the window is dragged to a new size, layout snaps to each size with no animation.
            for name in [NSWindow.willStartLiveResizeNotification, NSWindow.didEndLiveResizeNotification] {
                let starting = name == NSWindow.willStartLiveResizeNotification
                observers.append(center.addObserver(forName: name, object: window, queue: .main) { [weak self] _ in
                    MainActor.assumeIsolated { self?.shell?.liveResizing = starting }
                })
            }
            // Occlusion covers minimized, fully covered, another Space and a hidden app, and has the final say.
            observers.append(center.addObserver(forName: NSWindow.didChangeOcclusionStateNotification, object: window, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated {
                    self?.revealing = false
                    self?.updateShown()
                }
            })
            // Going away ends a reveal at once. Coming back is left to occlusion.
            let going: Set<Notification.Name> = [NSWindow.didMiniaturizeNotification, NSApplication.didHideNotification]
            for (name, object) in [(NSWindow.didMiniaturizeNotification, window), (NSWindow.didDeminiaturizeNotification, window),
                                   (NSApplication.didHideNotification, nil), (NSApplication.didUnhideNotification, nil)] as [(Notification.Name, NSWindow?)] {
                let leaving = going.contains(name)
                observers.append(center.addObserver(forName: name, object: object, queue: .main) { [weak self] _ in
                    MainActor.assumeIsolated {
                        if leaving { self?.revealing = false }
                        self?.updateShown()
                    }
                })
            }
            // With the mini player setting on, minimizing opens it and leaves the window in the Dock. The window coming
            // back closes it.
            observers.append(center.addObserver(forName: NSWindow.didMiniaturizeNotification, object: window, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated {
                    guard let shell = self?.shell, shell.miniOnMinimize else { return }
                    MiniPlayer.shared.open(shell: shell, forMinimize: true)
                }
            })
            observers.append(center.addObserver(forName: NSWindow.didDeminiaturizeNotification, object: window, queue: .main) { _ in
                MainActor.assumeIsolated {
                    if MiniPlayer.shared.openedForMinimize { MiniPlayer.shared.close() }
                }
            })
            // Occlusion only reports visible once a restore from the Dock has finished animating, and the animation shows
            // whatever the window last drew. So catch up as the window starts coming back, on unhide and on AppKit's
            // undocumented "will become visible" notice. It's observed by name, so without it occlusion still works.
            observers.append(center.addObserver(forName: NSApplication.willUnhideNotification, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.startReveal() }
            })
            observers.append(center.addObserver(forName: NSNotification.Name("_NSWindowWillBecomeVisible"), object: window, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.startReveal() }
            })
            updateShown()
        }

        /// The window is coming back. Views stay live until occlusion says otherwise, or a second at most in case it
        /// comes back covered and occlusion never changes.
        private var revealing = false
        private var revealCheck: Task<Void, Never>?

        private func startReveal() {
            revealing = true
            updateShown()
            revealCheck?.cancel()
            revealCheck = Task { [weak self] in
                try? await Task.sleep(for: .seconds(1))
                guard !Task.isCancelled, let self else { return }
                self.revealing = false
                self.updateShown()
            }
        }

        /// Whether anyone can see the window. Nothing visual runs while they can't.
        private func updateShown() {
            guard let window, let shell else { return }
            let visible = window.occlusionState.contains(.visible) && !window.isMiniaturized && !NSApp.isHidden
            shell.setWindowShown(visible || revealing)
        }

        func applyBackground() {
            guard let window, window.isOpaque == translucent || window.backgroundColor != background else { return }
            window.isOpaque = !translucent
            window.backgroundColor = background
            window.invalidateShadow()
        }

        private var background: NSColor { translucent ? .clear : T.bg.ns }

        /// Lines the window buttons up with the top bar's controls.
        private func placeButtons(in window: NSWindow) {
            let kinds: [NSWindow.ButtonType] = [.closeButton, .miniaturizeButton, .zoomButton]
            for (index, kind) in kinds.enumerated() {
                guard let button = window.standardWindowButton(kind), let parent = button.superview else { continue }
                let y = parent.bounds.height - 34 - button.frame.height / 2
                button.setFrameOrigin(NSPoint(x: 26 + CGFloat(index) * 23, y: y))
            }
        }
    }
}
