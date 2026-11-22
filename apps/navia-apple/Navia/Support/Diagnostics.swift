#if DEBUG
import AppKit
import SwiftUI

/// Self-checks for builds run over SSH, where nothing can click the window. Launch with
/// `-naviaDiagnostics /path/prefix` to write `prefix.json` and `prefix.png` and quit.
/// `-naviaDiagnosticsStay YES` keeps it open so the window can be captured.
struct DiagnosticsRunner: NSViewRepresentable {
    let shell: ShellModel

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        if let path = UserDefaults.standard.string(forKey: "naviaTranslucencyCheck") {
            Task { @MainActor [weak view] in
                try? await Task.sleep(for: .seconds(3))
                guard let window = view?.window else { return }
                await Self.checkTranslucency(window: window, shell: shell, path: path)
            }
            return view
        }
        if let dir = UserDefaults.standard.string(forKey: "naviaPages") {
            Task { @MainActor [weak view] in
                try? await Task.sleep(for: .seconds(3))
                guard let window = view?.window else { NSApp.terminate(nil); return }
                await Self.runPages(window: window, shell: shell, dir: dir)
            }
            return view
        }
        guard let prefix = UserDefaults.standard.string(forKey: "naviaDiagnostics") else { return view }
        Task { @MainActor [weak view] in
            try? await Task.sleep(for: .seconds(3))
            guard let window = view?.window else { NSApp.terminate(nil); return }
            await Self.run(window: window, shell: shell, prefix: prefix)
        }
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {}

    private static func find<V: NSView>(_ type: V.Type, in view: NSView) -> V? {
        if let match = view as? V { return match }
        for sub in view.subviews { if let found = find(type, in: sub) { return found } }
        return nil
    }

    private static func describeHit(_ root: NSView, _ point: NSPoint) -> String {
        guard let hit = root.hitTest(point) else { return "nil" }
        return String(describing: type(of: hit))
    }

    /// Draws the interface into an image without reading the screen, so it needs no permission. Platform glass
    /// doesn't render here, so glass areas differ from the real window.
    private static func writeSnapshot(of window: NSWindow, shell: ShellModel, to url: URL) {
        guard let content = window.contentView else { return }
        let size = content.bounds.size
        let scale = window.backingScaleFactor
        let renderer = ImageRenderer(content: RootView(shell: shell).environment(\.isSnapshot, true).frame(width: size.width, height: size.height))
        renderer.scale = scale
        guard let top = renderer.cgImage,
              let context = CGContext(data: nil, width: top.width, height: top.height, bitsPerComponent: 8, bytesPerRow: 0,
                                      space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue) else { return }
        context.setFillColor(T.bg.ns.cgColor)
        context.fill(CGRect(x: 0, y: 0, width: top.width, height: top.height))
        if let scroll = find(SongsNSTableView.self, in: content)?.enclosingScrollView,
           let rep = scroll.bitmapImageRepForCachingDisplay(in: scroll.bounds) {
            scroll.cacheDisplay(in: scroll.bounds, to: rep)
            let frame = scroll.convert(scroll.bounds, to: content)
            let y = content.isFlipped ? size.height - frame.maxY : frame.minY
            if let image = rep.cgImage {
                context.draw(image, in: CGRect(x: frame.minX * scale, y: y * scale, width: frame.width * scale, height: frame.height * scale))
            }
        }
        context.draw(top, in: CGRect(x: 0, y: 0, width: top.width, height: top.height))
        for (kind, color) in [(NSWindow.ButtonType.closeButton, NSColor.systemRed), (.miniaturizeButton, .systemYellow), (.zoomButton, .systemGreen)] {
            guard let button = window.standardWindowButton(kind) else { continue }
            let r = button.convert(button.bounds, to: nil)
            context.setFillColor(color.cgColor)
            context.fillEllipse(in: CGRect(x: r.minX * scale, y: r.minY * scale, width: r.width * scale, height: r.height * scale))
        }
        guard let image = context.makeImage() else { return }
        try? NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:])?.write(to: url)
    }

    /// Captures each page and view and runs its checks. `-naviaPagesOnly` limits it to a comma-separated list of steps.
    static func runPages(window: NSWindow, shell: ShellModel, dir: String) async {
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        let only = UserDefaults.standard.string(forKey: "naviaPagesOnly").map { Set($0.split(separator: ",").map(String.init)) }
        var report: [String: Any] = [:]
        func shot(_ name: String) async {
            try? await Task.sleep(for: .seconds(1))
            writeSnapshot(of: window, shell: shell, to: URL(fileURLWithPath: "\(dir)/\(name).png"))
        }
        func wanted(_ name: String) -> Bool { only?.contains(name) ?? true }

        let h = window.contentView!.bounds.height
        let w = window.contentView!.bounds.width
        func dockWidth() -> CGFloat { shell.dockPoints(windowWidth: window.contentView!.bounds.width, sidebar: shell.sidebarPreferred ? shell.sidebarPoints(windowWidth: window.contentView!.bounds.width, docked: true) : nil) }
        func dockLeft() -> CGFloat { window.contentView!.bounds.width - T.windowInset - dockWidth() }
        func switcherPoint(_ tab: Int) -> NSPoint { // 0 lyrics, 1 artwork, 2 queue. Window coordinates are bottom-left.
            NSPoint(x: dockLeft() + dockWidth() / 2 + CGFloat(tab - 1) * T.px(50), y: h - T.windowInset - T.px(38))
        }

        if wanted("player") {
            shell.go(.songs(genre: nil))
            shell.playerMode = .dock
            shell.playerView = .artwork
            await shot("dock-artwork")

            await click(window, at: switcherPoint(0))
            report["switcherLyricsClick"] = shell.playerView == .lyrics
            await shot("dock-lyrics")

            await click(window, at: switcherPoint(2))
            report["switcherQueueClick"] = shell.playerView == .queue
            report["queueUpcoming"] = shell.player.upcomingCount
            await shot("dock-queue")

            // Real hover and clicks on the queue rows. A double-click plays the row.
            let queueX = dockLeft() + dockWidth() / 2
            let firstRowY = h - T.windowInset - T.px(76) - T.px(8) - T.px(6) - T.px(30) - T.px(29)
            let before = shell.player.current?.id
            await click(window, at: NSPoint(x: queueX, y: firstRowY))
            report["queueSingleClickDidNotPlay"] = shell.player.current?.id == before
            await click(window, at: NSPoint(x: queueX, y: firstRowY), count: 2)
            report["queueDoubleClickPlayed"] = shell.player.current?.id != before
            if shell.player.isPlaying { shell.player.togglePlayPause() }

            shell.playerView = .lyrics
            try? await Task.sleep(for: .milliseconds(300))
            await click(window, at: NSPoint(x: dockLeft() + T.px(12) + T.iconW / 2, y: h - T.windowInset - T.px(38)))
            try? await Task.sleep(for: .seconds(1))
            report["expandClicked"] = shell.expanded
            await shot("expanded-lyrics")
            shell.playerView = .queue
            await shot("expanded-queue")
            shell.playerView = .artwork
            await shot("expanded-artwork")
            shell.expanded = false
            try? await Task.sleep(for: .milliseconds(500))
            shell.playerMode = .bar
            await shot("bar")
            shell.playerMode = .dock
            try? await Task.sleep(for: .milliseconds(400))

            let handleY = h / 2
            let widthBefore = shell.dockWidthSaved
            await drag(window, from: NSPoint(x: dockLeft(), y: handleY), to: NSPoint(x: dockLeft() - 60, y: handleY))
            report["dragNewWidth"] = shell.dockWidthSaved
            report["dragWidened"] = shell.dockWidthSaved > widthBefore
            await drag(window, from: NSPoint(x: dockLeft(), y: handleY), to: NSPoint(x: dockLeft() + 2000, y: handleY))
            report["dragClampedAtMin"] = shell.dockWidthSaved == T.dockMinW
            await click(window, at: NSPoint(x: dockLeft(), y: handleY), count: 2)
            try? await Task.sleep(for: .milliseconds(300))
            report["doubleClickReset"] = shell.dockWidthSaved == T.dockW

            shell.go(.songs(genre: nil))
            try? await Task.sleep(for: .seconds(1))
            if let table = find(SongsNSTableView.self, in: window.contentView!), table.numberOfRows > 6 {
                window.makeKey() // Key inside this app only. A background app's table ignores a plain first click.
                window.makeFirstResponder(table)
                try? await Task.sleep(for: .milliseconds(300))
                // Song n is table row `lead + n`, since the headers are rows above the songs.
                let lead = table.controller?.leadRows ?? 0
                func at(_ song: Int) -> NSPoint { let r = table.rect(ofRow: lead + song); return table.convert(NSPoint(x: shell.sidebarPoints(windowWidth: w, docked: shell.player.current != nil) + T.gutter + r.width / 3, y: r.midY), to: nil) }
                let before = shell.player.current?.id
                await click(window, at: at(1))
                report["rowClickSelects"] = table.selectedRowIndexes == IndexSet(integer: lead + 1)
                report["rowClickDoesNotPlay"] = shell.player.current?.id == before
                await click(window, at: at(3), modifiers: .control)
                report["ctrlClickAdds"] = table.selectedRowIndexes == IndexSet([lead + 1, lead + 3])
                await click(window, at: at(5), modifiers: .shift)
                report["shiftExtends"] = table.selectedRowIndexes.count >= 3
                report["shiftSelection"] = Array(table.selectedRowIndexes)
                await click(window, at: at(2))
                await click(window, at: at(2), count: 2)
                report["doubleClickPlays"] = shell.player.current?.id != before
                if shell.player.isPlaying { shell.player.togglePlayPause() }
                let keyPlay = shell.player.current?.id
                await click(window, at: at(4))
                if let key = NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil, characters: "\r", charactersIgnoringModifiers: "\r", isARepeat: false, keyCode: 36) {
                    window.sendEvent(key)
                    try? await Task.sleep(for: .milliseconds(300))
                    report["enterPlaysSelected"] = shell.player.current?.id != keyPlay
                    if shell.player.isPlaying { shell.player.togglePlayPause() }
                }
                await click(window, at: at(1))
                if let menu = table.controller?.menu(forRow: 1) { report["rowMenuItems"] = menu.items.map(\.title) }
                if let away = NSEvent.mouseEvent(with: .leftMouseDown, location: NSPoint(x: T.windowInset + 4, y: h - T.windowInset - 2), modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1) {
                    NSApp.sendEvent(away) // Local event monitors see this path, not window.sendEvent.
                    try? await Task.sleep(for: .milliseconds(200))
                    report["clickAwayClears"] = table.selectedRowIndexes.isEmpty
                }
                // A real Space press, outside any text field.
                let wasPlaying = shell.player.isPlaying
                if let key = NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil, characters: " ", charactersIgnoringModifiers: " ", isARepeat: false, keyCode: 49) {
                    _ = NSApp.sendEvent(key)
                    try? await Task.sleep(for: .milliseconds(200))
                    report["spaceToggles"] = shell.player.isPlaying != wasPlaying
                    if shell.player.isPlaying != wasPlaying { shell.player.togglePlayPause() }
                }
            }
            shell.go(.home)
            await shot("home-after-resize-reset")
            await drag(window, from: NSPoint(x: dockLeft(), y: handleY), to: NSPoint(x: dockLeft() + 2000, y: handleY))
            await shot("home-dock-min")
            await click(window, at: NSPoint(x: dockLeft(), y: handleY), count: 2)
            try? await Task.sleep(for: .milliseconds(300))
        }

        for (name, route): (String, Route) in [("home", .home), ("albums", .albums(.all)),
                                               ("album", .album(sampleAlbum(shell))), ("artists", .artists), ("artist", .artist(sampleArtist(shell))),
                                               ("playlists", .playlists), ("playlist", .playlist(samplePlaylist(shell))),
                                               ("browse", .browse()), ("genres", .browse(genres: true)), ("search", .search("mal")), ("discover", .discover(.forYou)),
                                               ("new", .discover(.new)), ("charts", .discover(.charts)), ("radio", .discover(.radio)),
                                               ("genre", .songs(genre: shell.library.genres.first ?? "Pop"))] {
            guard wanted(name) else { continue }
            shell.go(route)
            await shot(name)
        }

        // The mini player at each size, drawn alone. `-naviaForceHover ScrubChip,MiniChrome` shows the hover parts.
        for (name, size) in [("compact", MiniSize.compact), ("stacked-min", NSSize(width: MiniSize.compact.width, height: T.miniStackH.rounded())),
                             ("large", MiniSize.large), ("max", MiniSize.max)] where wanted("mini-\(name)") {
            let renderer = ImageRenderer(content: MiniPlayerView(shell: shell, mini: MiniPlayer.shared).environment(\.isSnapshot, true).frame(width: size.width, height: size.height))
            renderer.scale = window.backingScaleFactor
            if let image = renderer.cgImage {
                try? NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: "\(dir)/mini-\(name).png"))
            }
        }

        for layout in ["grid", "split"] where wanted("artist-albums-\(layout)") {
            UserDefaults.standard.set(layout, forKey: "albumLayout")
            shell.go(.albums(.artist(sampleArtist(shell), .albums)))
            await shot("artist-albums-\(layout)")
        }

        report["contentSize"] = NSStringFromSize(NSSize(width: w, height: h))
        if let data = try? JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys]) {
            try? data.write(to: URL(fileURLWithPath: dir + "/report.json"))
        }
        if !UserDefaults.standard.bool(forKey: "naviaDiagnosticsStay") { NSApp.terminate(nil) }
    }

    private static func sampleAlbum(_ shell: ShellModel) -> String { shell.player.current?.albumID ?? "" }
    private static func sampleArtist(_ shell: ShellModel) -> String { shell.library.topArtists(limit: 2).last?.id ?? "" }
    private static func samplePlaylist(_ shell: ShellModel) -> String { shell.library.playlists.first?.id ?? "" }

    private static func drag(_ window: NSWindow, from start: NSPoint, to end: NSPoint) async {
        func send(_ type: NSEvent.EventType, _ point: NSPoint) {
            guard let event = NSEvent.mouseEvent(with: type, location: point, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1) else { return }
            window.sendEvent(event)
        }
        send(.leftMouseDown, start)
        try? await Task.sleep(for: .milliseconds(80))
        for step in 1...6 {
            let t = CGFloat(step) / 6
            send(.leftMouseDragged, NSPoint(x: start.x + (end.x - start.x) * t, y: start.y + (end.y - start.y) * t))
            try? await Task.sleep(for: .milliseconds(60))
        }
        send(.leftMouseUp, end)
        try? await Task.sleep(for: .milliseconds(300))
    }

    /// straight to this window, so no event tap or Accessibility access is involved.
    private static func click(_ window: NSWindow, at point: NSPoint, count: Int = 1, modifiers: NSEvent.ModifierFlags = []) async {
        func make(_ type: NSEvent.EventType) -> NSEvent? {
            NSEvent.mouseEvent(with: type, location: point, modifierFlags: modifiers, timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: count, pressure: 1)
        }
        guard let down = make(.leftMouseDown), let up = make(.leftMouseUp) else { return }
        // Queued first, so a control's tracking loop finds the release instead of waiting for it.
        NSApp.postEvent(up, atStart: false)
        window.sendEvent(down)
        try? await Task.sleep(for: .milliseconds(420))
    }

    private static func describeFrames(of root: NSView, named name: String) -> [String] {
        var out: [String] = []
        func walk(_ view: NSView) {
            if String(describing: type(of: view)).contains(name) {
                out.append(NSStringFromRect(view.convert(view.bounds, to: nil)))
            }
            view.subviews.forEach(walk)
        }
        walk(root)
        return out
    }

    static var translucencySwitchFrame: CGRect?

    /// Large layers that paint solid. Anything here hides the blur.
    private static func solidLayers(under root: NSView) -> [String] {
        var out: [String] = []
        let minArea = root.bounds.width * root.bounds.height * 0.25
        func walk(_ layer: CALayer) {
            if let color = layer.backgroundColor, color.alpha > 0.9, layer.bounds.width * layer.bounds.height >= minArea {
                var chain: [String] = []
                var at: CALayer? = layer
                while let current = at {
                    chain.append("\(type(of: current))/\(current.delegate.map { String(describing: type(of: $0)) } ?? "-")")
                    at = current.superlayer
                }
                let sub = layer.sublayers?.count ?? 0
                out.append("\(NSStringFromRect(layer.bounds)) comps=\(color.components ?? []) opacity=\(layer.opacity) hidden=\(layer.isHidden) filter=\(layer.compositingFilter.map { "\($0)" } ?? "-") subs=\(sub) chain=\(chain.joined(separator: " < "))")
            }
            layer.sublayers?.forEach(walk)
        }
        if let layer = root.layer { walk(layer) }
        return out
    }

    private static func effectViews(in root: NSView) -> [NSVisualEffectView] {
        var out: [NSVisualEffectView] = []
        func walk(_ view: NSView) {
            if let effect = view as? NSVisualEffectView { out.append(effect) }
            view.subviews.forEach(walk)
        }
        walk(root)
        return out
    }

    private static func translucencyState(_ window: NSWindow, _ shell: ShellModel) async -> [String: Any] {
        try? await Task.sleep(for: .milliseconds(400))
        let root = window.contentView!.superview!
        var state: [String: Any] = [
            "saved": shell.windowTranslucency, "reduce": shell.systemReduceTransparency, "fullScreen": shell.inFullScreen,
            "active": shell.translucencyActive, "isOpaque": window.isOpaque, "backgroundAlpha": window.backgroundColor.alphaComponent,
            "background": "\(window.backgroundColor.usingColorSpace(.sRGB) ?? window.backgroundColor)", "hasShadow": window.hasShadow,
            "resizable": window.styleMask.contains(.resizable), "solidLayers": solidLayers(under: root),
        ]
        let blur = effectViews(in: window.contentView!).filter { $0.blendingMode == .behindWindow }
        state["behindWindowEffects"] = blur.map {
            ["material": $0.material.rawValue, "state": $0.state.rawValue, "frame": NSStringFromRect($0.convert($0.bounds, to: nil)), "appearance": $0.effectiveAppearance.name.rawValue]
        }
        for (name, kind) in [("close", NSWindow.ButtonType.closeButton), ("miniaturize", .miniaturizeButton), ("zoom", .zoomButton)] {
            if let button = window.standardWindowButton(kind) {
                state[name] = ["frame": NSStringFromRect(button.convert(button.bounds, to: nil)), "hidden": button.isHidden, "enabled": button.isEnabled]
            }
        }
        return state
    }

    /// Flips everything that decides translucency and records the window after each step. Restores the saved choice.
    static func checkTranslucency(window: NSWindow, shell: ShellModel, path: String) async {
        let defaults = UserDefaults.standard
        let hadSavedValue = defaults.persistentDomain(forName: Bundle.main.bundleIdentifier!)?["windowTranslucency"] != nil
        let started = shell.windowTranslucency
        var report: [String: Any] = [:]
        report["contentBounds"] = NSStringFromRect(window.contentView!.bounds)
        report["themeFrameLayerCornerRadius"] = window.contentView!.superview!.layer?.cornerRadius ?? -1
        report["1_launch"] = await translucencyState(window, shell)
        shell.windowTranslucency = false
        report["2_saved_off"] = await translucencyState(window, shell)
        shell.windowTranslucency = true
        report["3_saved_on"] = await translucencyState(window, shell)
        shell.systemReduceTransparency = true
        report["4_reduce_on"] = await translucencyState(window, shell)
        shell.systemReduceTransparency = NSWorkspace.shared.accessibilityDisplayShouldReduceTransparency
        shell.inFullScreen = true
        report["5_fullscreen"] = await translucencyState(window, shell)
        shell.inFullScreen = false

        let route = shell.route
        shell.go(.settings)
        try? await Task.sleep(for: .milliseconds(600))
        if let frame = translucencySwitchFrame {
            report["switchFrame"] = NSStringFromRect(frame)
            let point = NSPoint(x: frame.midX, y: window.contentView!.bounds.height - frame.midY)
            let before = shell.windowTranslucency
            await click(window, at: point)
            report["clickChanged"] = shell.windowTranslucency != before
            report["6_after_click"] = await translucencyState(window, shell)
            await click(window, at: point)
            report["clickRestored"] = shell.windowTranslucency == before
        }
        shell.go(route)

        let frame = window.frame
        window.setFrame(NSRect(x: frame.minX, y: frame.minY, width: frame.width - 200, height: frame.height - 120), display: true)
        report["7_resized"] = await translucencyState(window, shell)
        report["contentBoundsResized"] = NSStringFromRect(window.contentView!.bounds)
        window.setFrame(frame, display: true)
        report["8_end"] = await translucencyState(window, shell)

        shell.windowTranslucency = started
        if !hadSavedValue { defaults.removeObject(forKey: "windowTranslucency") }
        if let data = try? JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys]) {
            try? data.write(to: URL(fileURLWithPath: path))
        }
        if !defaults.bool(forKey: "naviaDiagnosticsStay") { NSApp.terminate(nil) }
    }

    static func run(window: NSWindow, shell: ShellModel, prefix: String) async {
        var report: [String: Any] = ["windowNumber": window.windowNumber, "frame": NSStringFromRect(window.frame)]
        let root = window.contentView!.superview!

        // coordinates are from the bottom-left.
        let h = root.bounds.height, w = root.bounds.width
        report["hitTopBarBack"] = describeHit(root, NSPoint(x: shell.sidebarPoints(windowWidth: w, docked: shell.player.current != nil) + T.windowInset + T.gutter + 20, y: h - T.topbarH / 2))
        report["hitTopBarCenter"] = describeHit(root, NSPoint(x: w / 2, y: h - T.topbarH / 2))
        report["hitSidebarToggle"] = describeHit(root, NSPoint(x: T.windowInset + shell.sidebarPoints(windowWidth: w, docked: shell.player.current != nil) - T.px(12) - T.iconW / 2, y: h - T.windowInset - T.px(31)))
        for (name, kind) in [("close", NSWindow.ButtonType.closeButton), ("zoom", .zoomButton)] {
            if let button = window.standardWindowButton(kind) {
                report["\(name)Button"] = NSStringFromRect(button.convert(button.bounds, to: nil))
            }
        }

        if let table = find(SongsNSTableView.self, in: root), let scroll = table.enclosingScrollView {
            report["rows"] = table.numberOfRows
            report["rowHeight"] = table.rowHeight
            report["tableFrame"] = NSStringFromRect(table.frame)
            if let first = table.controller?.tableRow(0), table.controller?.songIndex(first) != nil, let cell = table.view(atColumn: 0, row: first, makeIfNecessary: true) {
                let name = cell.subviews.compactMap { $0 as? NSTextField }.first
                report["cellFont"] = name?.font.map { "\($0.fontName) \($0.pointSize)" } ?? "?"
            }
            var samples: [Double] = []
            let contentH = table.frame.height
            let pageH = scroll.contentView.bounds.height
            var y: CGFloat = 0
            while y < contentH, samples.count < 400 {
                let start = CACurrentMediaTime()
                scroll.contentView.scroll(to: NSPoint(x: 0, y: y))
                scroll.reflectScrolledClipView(scroll.contentView)
                table.layoutSubtreeIfNeeded()
                samples.append((CACurrentMediaTime() - start) * 1000)
                y += max(pageH, contentH / 400)
            }
            samples.sort()
            if !samples.isEmpty {
                report["scrollPages"] = samples.count
                report["scrollMedianMs"] = samples[samples.count / 2]
                report["scrollWorstMs"] = samples.last!
            }
            scroll.contentView.scroll(to: .zero)
            scroll.reflectScrolledClipView(scroll.contentView)
            table.layoutSubtreeIfNeeded()
        }

        // Real click routing, sent into this window only. Covers the sidebar toggle over its scroll view, and a table row.
        let before = shell.sidebarPreferred
        await click(window, at: NSPoint(x: T.windowInset + shell.sidebarPoints(windowWidth: w, docked: shell.player.current != nil) - T.px(12) - T.iconW / 2, y: h - T.windowInset - T.px(31)))
        report["clickSidebarToggleWorked"] = shell.sidebarPreferred != before
        shell.sidebarPreferred = before
        let modeBefore = shell.playerMode
        await click(window, at: NSPoint(x: 1948 * 0.7, y: h - 60 * 0.7))
        report["clickDockBarButtonWorked"] = shell.playerMode != modeBefore
        shell.playerMode = modeBefore
        try? await Task.sleep(for: .milliseconds(300))
        let compactBefore = shell.compactRows
        await click(window, at: NSPoint(x: 1135 * 0.7, y: h - 48 * 0.7))
        report["clickCompactToggleWorked"] = shell.compactRows != compactBefore
        shell.compactRows = compactBefore
        try? await Task.sleep(for: .milliseconds(300))
        if let table = find(SongsNSTableView.self, in: root), table.numberOfRows > 5 {
            let rect = table.rect(ofRow: (table.controller?.leadRows ?? 0) + 3)
            let point = table.convert(NSPoint(x: rect.midX, y: rect.midY), to: nil)
            let previous = shell.player.current?.id
            await click(window, at: point)
            report["clickRowSelected"] = table.controller?.songIndex(table.selectedRow) ?? -1
            await click(window, at: point, count: 2)
            report["doubleClickPlayed"] = shell.player.current?.id != previous
            report["nowPlaying"] = shell.player.current?.title ?? ""
            if shell.player.isPlaying { shell.player.togglePlayPause() }
        }
        let songs = shell.songs
        let start = CACurrentMediaTime()
        songs.query = "billie"
        report["searchMs"] = (CACurrentMediaTime() - start) * 1000
        report["searchBillieCount"] = songs.count
        songs.sort = .title
        report["firstByTitle"] = songs.song(at: 0)?.title ?? ""
        songs.query = ""
        songs.sort = .plays
        report["countAfterClear"] = songs.count
        report["accent"] = "\(shell.theme.accent)"
        report["onAccent"] = "\(shell.theme.onAccent)"
        report["hostingScrollFrames"] = describeFrames(of: root, named: "HostingScrollView")
        try? await Task.sleep(for: .seconds(1)) // Lets artwork for the visible rows finish loading.
        // The window isn't key when launched in the background, so show a focused selection by hand.
        if let table = find(SongsNSTableView.self, in: root), let controller = table.controller, table.numberOfRows > 2 {
            let row = controller.tableRow(1)
            table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
            controller.toggleFavorite(row: 1)
            controller.toggleFavorite(row: 1)
            report["selectionKeptAfterFavorite"] = table.selectedRow == row
            table.rowView(atRow: row, makeIfNecessary: false)?.isEmphasized = true
            (table.view(atColumn: 0, row: row, makeIfNecessary: false) as? NSTableCellView)?.backgroundStyle = .emphasized
            table.rowView(atRow: row, makeIfNecessary: false)?.needsDisplay = true
            try? await Task.sleep(for: .milliseconds(300))
        }
        writeSnapshot(of: window, shell: shell, to: URL(fileURLWithPath: prefix + ".png"))
        // In a narrow window the dock should give way to the bar, then come back.
        let frame = window.frame
        window.setFrame(NSRect(x: frame.minX, y: frame.minY, width: 980, height: frame.height), display: true)
        try? await Task.sleep(for: .seconds(1))
        report["narrowCloseButton"] = window.standardWindowButton(.closeButton).map { NSStringFromRect($0.convert($0.bounds, to: nil)) } ?? ""
        writeSnapshot(of: window, shell: shell, to: URL(fileURLWithPath: prefix + "-narrow.png"))
        window.setFrame(frame, display: true)
        try? await Task.sleep(for: .seconds(1))

        if let data = try? JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys]) {
            try? data.write(to: URL(fileURLWithPath: prefix + ".json"))
        }
        if !UserDefaults.standard.bool(forKey: "naviaDiagnosticsStay") { NSApp.terminate(nil) }
    }
}
#endif
