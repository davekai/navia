#if DEBUG
import AppKit
import Foundation
import NaviaCoreFFI

/// `-naviaCoreCheck <report.json>` drives the real core without a display, running the steps in `-naviaCoreSteps`
/// and recording timing, memory and player state. Steps are comma separated, and each `case` below documents its
/// own. Reports are appended to the JSON file as they happen.
/// For CPU and wakeup measurements use a fixed step list with `wait=`, since `driver=` polls its file every 50 ms.
@MainActor enum CoreCheck {
    static func describe(_ q: AudioQuality?) -> String {
        guard let q else { return "none" }
        return "\(q.label(short: false)) | \(q.detailLine) | source=\(q.source.map { "\($0)" } ?? "unknown") transcode=\(q.transcodeFormat ?? "-") \(q.transcodeKbps.map(String.init) ?? "")"
    }

    private static var results: [[String: Any]] = []
    private static var pocketChanges: [[String: Any]] = []
    private static var pocketObserver: CFRunLoopObserver?
    private static var layerChanges: [[String: Any]] = []
    private static var layerObserver: CFRunLoopObserver?

    /// Every titlebar, toolbar, scroll pocket, backdrop or material layer under `root`, keyed by its path of class
    /// names, with the state that would show on screen.
    private static func edgeLayerStates(_ root: CALayer) -> [String: String] {
        var out: [String: String] = [:]
        let words = ["Titlebar", "Toolbar", "Pocket", "Backdrop", "VisualEffect", "Material", "Glass", "Blur", "Edge"]
        func walk(_ layer: CALayer, path: String, inside: Bool) {
            let name = String(describing: type(of: layer))
            let owner = layer.delegate.map { String(describing: type(of: $0)).prefix(40) } ?? ""
            let tagged = inside || words.contains { name.contains($0) || owner.contains($0) }
            let short = path.split(separator: "/").suffix(2).joined(separator: "/") + "/" + (owner.isEmpty ? name : owner + ":" + name)
            // Keyed by identity, so a sibling inserted before it doesn't read as this layer being replaced.
            let key = String(UInt(bitPattern: ObjectIdentifier(layer).hashValue) % 1_000_000) + " " + short
            if tagged {
                let shown = layer.presentation()?.opacity ?? layer.opacity
                let anims = layer.animationKeys()?.joined(separator: "+") ?? ""
                let f = layer.bounds
                out[key] = "\(layer.isHidden ? "H" : "v") o=\(String(format: "%.2f", layer.opacity))/\(String(format: "%.2f", shown)) \(Int(f.width))x\(Int(f.height))\(anims.isEmpty ? "" : " anim=" + anims)\(layer.filters?.isEmpty == false ? " f\(layer.filters!.count)" : "")"
            }
            for sub in layer.sublayers ?? [] where (sub.sublayers?.count ?? 0) < 400 {
                walk(sub, path: short, inside: tagged)
            }
        }
        walk(root, path: "", inside: false)
        return out
    }

    static func startIfRequested(shell: ShellModel) {
        guard let path = UserDefaults.standard.string(forKey: "naviaCoreCheck") else { return }
        let steps = (UserDefaults.standard.string(forKey: "naviaCoreSteps") ?? "waitList,report=boot").split(separator: ",").map(String.init)
        Task { await run(shell: shell, steps: steps, path: path) }
    }

    private static func run(shell: ShellModel, steps: [String], path: String) async {
        guard let library = shell.library as? CoreLibraryModel, let player = shell.player as? CorePlayerModel else {
            write(path, ["error": "not running on the real core"])
            NSApp.terminate(nil)
            return
        }
        // `-naviaCheckKeepFocus YES`: real clicks need the app active.
        if !UserDefaults.standard.bool(forKey: "naviaCheckKeepFocus") { keepFocusAway() } else { logClicks() }
        for step in steps {
            current = step
            await perform(step, shell: shell, library: library, player: player, path: path)
            write(path, ["results": results])
        }
        write(path, ["results": results, "finished": true])
    }

    private static var current = ""
    /// real clicks while focus is kept (`clickLog`): each button event's own time and when the app got it.
    private static var clicks: [[String: Any]] = []
    private static var clickMonitor: Any?

    private static func logClicks() {
        clickMonitor = NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .leftMouseUp]) { event in
            MainActor.assumeIsolated {
                clicks.append(["type": event.type == .leftMouseDown ? "down" : "up", "event": event.timestamp, "got": CACurrentMediaTime(),
                               "active": NSApp.isActive])

            }
            return event
        }
    }
    private static var focusWatchers: [NSObjectProtocol] = []

    /// A check run hands focus straight back whenever it becomes active, noting the step running then (`focusTaken`).
    private static func keepFocusAway() {
        var previous = NSWorkspace.shared.frontmostApplication.flatMap { $0 == .current ? nil : $0 }
        focusWatchers.append(NSWorkspace.shared.notificationCenter.addObserver(forName: NSWorkspace.didActivateApplicationNotification, object: nil, queue: .main) { note in
            if let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication, app != .current { previous = app }
        })
        focusWatchers.append(NotificationCenter.default.addObserver(forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main) { _ in
            MainActor.assumeIsolated { results.append(["focusTaken": current, "returnedTo": previous?.localizedName ?? "none"]) }
            previous?.activate()
        })
    }

    /// `driver=<dir>`: runs steps appended to `<dir>/cmds`, counting them in `<dir>/done`, until `quit`.
    private static func driver(dir: String, shell: ShellModel, library: CoreLibraryModel, player: CorePlayerModel, path: String) async {
        var done = 0
        while true {
            let lines = ((try? String(contentsOfFile: dir + "/cmds", encoding: .utf8)) ?? "").split(separator: "\n").map(String.init)
            while done < lines.count {
                current = lines[done]
                await perform(lines[done], shell: shell, library: library, player: player, path: path)
                done += 1
                try? String(done).write(toFile: dir + "/done", atomically: true, encoding: .utf8)
                write(path, ["results": results])
            }
            try? await Task.sleep(for: .milliseconds(50))
        }
    }

    private static func perform(_ step: String, shell: ShellModel, library initialLibrary: CoreLibraryModel, player initialPlayer: CorePlayerModel, path: String) async {
            // Sign-in and sign-out replace the models, so steps always read the current ones.
            let library = (shell.library as? CoreLibraryModel) ?? initialLibrary
            let player = (shell.player as? CorePlayerModel) ?? initialPlayer
            let parts = step.split(separator: "=", maxSplits: 1).map(String.init)
            let (name, value) = (parts[0], parts.count > 1 ? parts[1] : "")
            switch name {
            case "driver": await driver(dir: value, shell: shell, library: library, player: player, path: path)
            case "activate":
                NSApp.activate(ignoringOtherApps: true)
                window?.makeKeyAndOrderFront(nil)
            // `peek[=show]`: the window on the second display, in front without focus, so it draws; with one display, see-through.
            case "peek":
                if let window {
                    if let second = NSScreen.screens.dropFirst().first {
                        guard value.isEmpty else { break }
                        let room = second.visibleFrame
                        let size = NSSize(width: min(window.frame.width, room.width), height: min(window.frame.height, room.height))
                        window.setFrame(NSRect(origin: NSPoint(x: room.minX, y: room.maxY - size.height), size: size), display: true)
                    } else {
                        window.ignoresMouseEvents = true
                        window.alphaValue = value == "show" ? 1 : 0.01
                    }
                    // on whichever Space the second display shows now, a full-screen one too, so it's really on screen.
                    if value.isEmpty { window.collectionBehavior.formUnion([.canJoinAllSpaces, .fullScreenAuxiliary]); window.orderFrontRegardless() }
                    // A launch can still make the check app active: hand the focus straight back.
                    if NSApp.isActive { NSApp.deactivate() }
                }
            case "window":
                let v = value.split(separator: ":").compactMap { Double($0) }
                if v.count == 2, let window { window.setContentSize(NSSize(width: v[0], height: v[1])) }
                recordWindow()
            // `resizeBench=W1:H1:W2:H2:FRAMES:LIVE`: a timed corner drag in code; LIVE 1 sets `liveResizing`.
            case "resizeBench":
                let v = value.split(separator: ":").compactMap { Double($0) }
                guard v.count == 6, let window else { break }
                if v[5] == 1 { shell.liveResizing = true }
                var times: [Double] = []
                let frames = Int(v[4])
                for i in 0..<frames {
                    let t = Double(i) / Double(max(1, frames - 1)) * 2
                    let k = t <= 1 ? t : 2 - t
                    let start = CACurrentMediaTime()
                    window.setContentSize(NSSize(width: v[0] + (v[2] - v[0]) * k, height: v[1] + (v[3] - v[1]) * k))
                    window.contentView?.layoutSubtreeIfNeeded()
                    window.displayIfNeeded()
                    CATransaction.flush()
                    times.append((CACurrentMediaTime() - start) * 1000)
                    try? await Task.sleep(for: .milliseconds(16))
                }
                shell.liveResizing = false
                let sorted = times.sorted()
                func at(_ q: Double) -> Double { sorted.isEmpty ? 0 : sorted[min(sorted.count - 1, Int(Double(sorted.count) * q))] }
                results.append(["resizeBench": value, "medianMs": at(0.5), "p90Ms": at(0.9), "maxMs": sorted.last ?? 0,
                                "meanMs": times.reduce(0, +) / Double(max(1, times.count))])
            case "windowinfo": recordWindow()
            case "mode": shell.playerMode = value == "bar" ? .bar : .dock
            case "playerview": shell.playerView = PlayerView(rawValue: value) ?? .artwork
            case "expanded": shell.expanded = value != "0"
            // `mini=open|close|toggle|large|compact|switch`, `miniSize=W:H`, `miniMove=X:Y` (bottom-left screen points),
            // `miniClick=X:Y` (from its top left), `miniReport=NAME`, `miniMenu`.
            case "mini":
                switch value {
                case "open": MiniPlayer.shared.open(shell: shell)
                case "close": MiniPlayer.shared.close()
                case "toggle": MiniPlayer.shared.toggle(shell: shell)
                case "large": MiniPlayer.shared.resize(to: MiniSize.large)
                case "switch": MiniPlayer.shared.switchSize()
                default: MiniPlayer.shared.resize(to: MiniSize.compact)
                }
            case "miniSize":
                let v = value.split(separator: ":").compactMap { Double($0) }
                if v.count == 2 { MiniPlayer.shared.resize(to: NSSize(width: v[0], height: v[1])) }
            case "miniMove":
                let v = value.split(separator: ":").compactMap { Double($0) }
                if v.count == 2 { MiniPlayer.shared.panel?.setFrameOrigin(NSPoint(x: v[0], y: v[1])) }
            case "miniClick": results.append(["miniClick": value, "sent": MiniCheck.click(value)])
            case "miniDrag": results.append(MiniCheck.drag(value))
            case "rightClickMenus": results.append(MiniCheck.rightClickMenus())
            case "miniReport": results.append(MiniCheck.report(value, shell: shell))
            case "miniMenu": results.append(["miniMenu": MiniCheck.menuItems()])
            case "songQuery": shell.songs.query = value
            case "radio":
                if let song = player.current { shell.startSongRadio(song) }
            case "serverRetry": player.retryServer()
            // `connectSet=<address>|<user>|<password>` fills the sign-in form. Reports never include the password.
            case "connectSet":
                if let form = shell.connect {
                    let v = value.split(separator: "|", omittingEmptySubsequences: false).map(String.init)
                    if v.count == 3 { form.address = v[0]; form.user = v[1]; form.password = v[2] }
                }
            case "connectTest": if let form = shell.connect { await form.test() }; connectReport(shell)
            case "connectSubmit": if let form = shell.connect { await form.connect(shell: shell) }; connectReport(shell)
            case "connectOpen": shell.openChangeServer(); connectReport(shell)
            case "connectCancel": shell.cancelConnect(); connectReport(shell)
            case "connectReport": connectReport(shell)
            // Signs in without waiting, so a capture can catch it running.
            case "connectSubmitLater": if let form = shell.connect { Task { await form.connect(shell: shell) } }; connectReport(shell)
            case "deezerAnswer": shell.answerDeezerStep(value == "on"); connectReport(shell)
            case "deezerReport": results.append(["deezerChoice": shell.deezerChoice.map { "\($0)" } ?? "unset"])
            case "signOut": shell.signOut(); connectReport(shell)
            case "accountReport":
                results.append(["accountKey": AccountPrefs.current, "keychainHasItem": LoginKeychain.exists, "sessionFile": CoreMode.sessionFile?.path ?? "", "signedIn": shell.activeLogin?.host ?? "", "origin": shell.activeLogin.map { "\($0.origin)" } ?? "none", "library": shell.library.serverLabel])
            case "keychainDelete": LoginKeychain.delete()
            case "downloadAllVisible":
                let songs = (library.core?.albumSongs(albumId: value).map { SongRow(core: $0) }) ?? []
                library.setDownloaded(songs: songs, true)
                results.append(["downloadAlbum": value, "songs": songs.count])
            case "downloadsReport":
                let entries = await library.downloadEntries()
                results.append(["downloadEntries": entries.map { ["title": $0.song.title, "bytes": $0.bytes] }, "downloadedCount": library.downloadedCount])
            case "clearDownloads":
                library.setDownloaded(songs: (await library.downloadEntries()).map(\.song), false)
            case "offlineReport": results.append(["offline": library.isOffline, "state": "\(library.state)", "waiting": player.isWaitingForServer])
            case "waitList":
                for _ in 0..<800 where library.allSongs.count == 0 { try? await Task.sleep(for: .milliseconds(25)) }
            case "waitSync":
                let start = CFAbsoluteTimeGetCurrent()
                for _ in 0..<2400 where AppTimeline.snapshot["syncDone"] == nil { try? await Task.sleep(for: .milliseconds(50)) }
                report(library, player, "syncWait", extra: ["waitedMs": (CFAbsoluteTimeGetCurrent() - start) * 1000], path, shell: shell)
            case "volume": player.volume = Double(value) ?? 0
            // `deviceVolume=0.36` or `deviceVolume=none` sets the AirPlay receiver volume.
            case "deviceVolume": AirPlayModel.shared.onRemote?(.volumeChanged(volume: Float(value)))
            case "mute": player.isMuted.toggle()
            case "playRow":
                let index = Int(value) ?? 0
                // Rows arrive in the background, so ask, then wait a moment.
                _ = library.allSongs.song(at: index)
                try? await Task.sleep(for: .milliseconds(300))
                player.play(songAt: index, in: library.allSongs)
            case "playAlbum":
                let id = value == "first" ? (library.topAlbums(limit: 1).first?.id ?? "") : value
                for _ in 0..<40 where id.isEmpty && library.topAlbums(limit: 1).isEmpty { try? await Task.sleep(for: .milliseconds(100)) }
                let albumID = value == "first" ? (library.topAlbums(limit: 1).first?.id ?? "") : value
                let songs = library.core?.albumSongs(albumId: albumID).map { SongRow(core: $0) } ?? []
                player.play(songs, at: 0, shuffle: false)
                results.append(["albumPlayed": albumID, "songs": songs.count])
            // `like=N` toggles the favorite of the library's Nth song. `likedReport` lists Liked Songs.
            case "like":
                let index = Int(value) ?? 0
                _ = library.allSongs.song(at: index)
                try? await Task.sleep(for: .milliseconds(300))
                if let song = library.allSongs.song(at: index) { library.toggleFavorite(song: song) }
            case "likedReport":
                let list = library.likedSongs()
                try? await Task.sleep(for: .milliseconds(500))
                results.append(["liked": list.entireList().map(\.title)])
            case "pause", "resume": player.togglePlayPause()
            case "bridgeOverlap": results.append(["step": "bridgeOverlap", "result": player.debugBridgeOverlap()])
            case "next": player.next()
            case "prev": player.previous()
            case "seek": player.seek(to: Double(value) ?? 0)
            case "shuffle": player.shuffle.toggle()
            // `repeat` cycles as the button does. `repeat=one` sets it.
            case "repeat": player.repeatMode = PlayerRepeat(rawValue: value) ?? player.repeatMode.next
            // `moveUp=1+4:0` moves Up Next offsets 1 and 4 before offset 0.
            case "moveUp":
                let parts = value.split(separator: ":")
                if parts.count == 2, let before = Int(parts[1]) {
                    player.moveUpcoming(parts[0].split(separator: "+").compactMap { Int($0) }, before: before)
                }
            // `queueOffer` waits up to 30 s for "Continue from" and reports it. `queueAnswer=accept|dismiss` answers it.
            // A check run never saves a queue.
            case "queueOffer":
                for _ in 0..<300 where player.queueOfferDevice == nil { try? await Task.sleep(for: .milliseconds(100)) }
                results.append(["queueOffer": player.queueOfferDevice ?? ""])
            case "queueAnswer":
                player.answerQueueOffer(accept: value == "accept")
                try? await Task.sleep(for: .milliseconds(800))
                results.append(["queueAnswer": value, "offer": player.queueOfferDevice ?? "", "current": player.current?.title ?? "", "upNext": player.upcomingCount, "position": player.position])
            // `queueReport` compares the app's queue with the core's. `queueVerify` checks the model, the app's mirror and
            // the player agree. `menuPlacement[=top]` moves the window to the screen's bottom or top, then reports where each
            // menu would open.
            case "menuPlacement":
                if let window, let visible = window.screen?.visibleFrame {
                    let y = value == "top" ? visible.maxY - window.frame.height : visible.minY
                    window.setFrameOrigin(NSPoint(x: window.frame.minX, y: y))
                    try? await Task.sleep(for: .milliseconds(400))
                    var rows: [[String: Any]] = []
                    func walk(_ view: NSView) {
                        if let button = view as? MenuOpeningView, let w = button.window {
                            let items = button.entries()
                            if !items.isEmpty {
                                let size = makeMenu(items).size
                                let place = button.placement(for: size)
                                let frame = w.convertToScreen(button.convert(button.bounds, to: nil))
                                let top = w.convertToScreen(button.convert(NSRect(origin: place.point, size: .zero), to: nil)).minY
                                rows.append(["first": items.first?.title ?? "", "buttonMinY": frame.minY, "buttonMaxY": frame.maxY, "menuH": size.height, "above": place.above,
                                             "menuTop": top, "menuBottom": top - size.height, "onScreen": top <= visible.maxY + 0.5 && top - size.height >= visible.minY - 0.5])
                            }
                        }
                        view.subviews.forEach(walk)
                    }
                    if let content = window.contentView { walk(content) }
                    results.append(["menuPlacement": rows, "visibleMinY": visible.minY, "visibleMaxY": visible.maxY])
                }
            case "queueVerify":
                try? await Task.sleep(for: .milliseconds(400))
                results.append(["queueVerify": player.debugVerify()])
            case "queueReport":
                try? await Task.sleep(for: .milliseconds(300))
                let upcoming = (0..<min(player.upcomingCount, 12)).compactMap { player.upcoming(at: $0)?.song.title }
                var core: [String] = []
                if let snap = player.debugSnapshot, let at = snap.index {
                    let ids = snap.queue.map(\.id)
                    let start = Int(at)
                    let tail = Array(ids.dropFirst(start + 1).prefix(12))
                    core = tail.map { id in library.core?.songById(id: id)?.title ?? id }
                }
                // Both queues around the current song, to check played queued songs left both.
                var coreAround: [String] = []
                if let snap = player.debugSnapshot, let at = snap.index.map(Int.init) {
                    coreAround = snap.queue.indices.filter { $0 >= at - 3 && $0 < at + 8 }.map { i in
                        (i == at ? "> " : "") + (snap.queue[i].queued ? "[q] " : "") + (library.core?.songById(id: snap.queue[i].id)?.title ?? snap.queue[i].id)
                    }
                }
                let mine = player.debugQueue
                results.append(["queueReport": ["current": player.current?.title ?? "", "upNext": upcoming, "coreUpNext": core, "count": player.upcomingCount, "shuffle": player.shuffle, "repeat": player.repeatMode.rawValue,
                                                "index": mine.index, "coreIndex": player.debugSnapshot?.index.map(Int.init) ?? -1, "around": mine.rows, "coreAround": coreAround, "nowPlaying": player.nowPlayingSnapshot]])
            // Autoplay: `autoplayReport`, `autoAdd=K`, `autoNext=K`, `autoRemove=K+K`, `autoMove=K+K:BEFORE`, `autoPlay=K`,
            // `jumpUp=I|last`, `autoplayOn=0|1`, `queueVisible=0|1`, `addSong=ID`.
            case "autoplayReport":
                try? await Task.sleep(for: .milliseconds(300))
                var core: [String] = []
                if let snap = player.debugSnapshot, let at = snap.index {
                    core = snap.queue.dropFirst(Int(at) + 1).prefix(16).map { library.core?.songById(id: $0.id)?.title ?? $0.id }
                }
                let shown: String = switch player.autoplayShown { case .on: "on"; case .finding: "finding"; case .hidden: "hidden"; case .repeating: "repeat" }
                results.append(["autoplayReport": [
                    "enabled": player.autoplayEnabled, "shown": shown, "count": player.autoplayCount, "basedOn": player.autoplayBasedOn ?? "",
                    "autoplay": (0..<player.autoplayCount).compactMap { player.autoplay(at: $0).map { "\($0.song.title) — \($0.song.artist)" } },
                    "current": player.current?.title ?? "", "upNext": (0..<min(player.upcomingCount, 16)).compactMap { player.upcoming(at: $0).map { ($0.queued ? "[q] " : "") + $0.song.title } },
                    "upCount": player.upcomingCount, "coreAfterCurrent": core, "status": player.isPlaying, "repeat": player.repeatMode.rawValue, "shuffle": player.shuffle,
                ] as [String: Any]])
            case "autoAdd", "autoNext":
                if let song = player.autoplay(at: Int(value) ?? 0)?.song { name == "autoAdd" ? player.addToQueue(song) : player.playNext(song) }
            // `qualityReport` gives the quality badge's facts.
            case "qualityReport":
                results.append(["quality": describe(player.quality)])
            // `addMenu=SONG_ID:FILTER` reports the Add to Playlist submenu, and what a typed filter leaves.
            case "addMenu":
                let parts = value.split(separator: ":", maxSplits: 1).map(String.init)
                guard let row = library.core?.songById(id: parts[0]) else { break }
                let menu = makeMenu(SongMenu.single(SongRow(core: row), shell: shell))
                guard let item = menu.items.first(where: { $0.submenu?.items.first?.view is MenuFindField }), let sub = item.submenu,
                      let find = sub.items.first?.view as? MenuFindField else { results.append(["addMenu": "no submenu"]); break }
                let shown = { sub.items.filter { !$0.isHidden && $0.view == nil }.map { $0.attributedTitle?.string.trimmingCharacters(in: .whitespaces) ?? $0.title } }
                let all = shown()
                find.debugType(parts.count > 1 ? parts[1] : "")
                results.append(["addMenu": ["row": item.title, "items": all.count, "first": Array(all.prefix(4)), "filtered": shown()]])
            // `menuShow=song:ID[+ID…]:SECONDS[:HOVER[:remove[:picker[:light|dark]]]]`, `menuShow=playlist:ID:SECONDS`, `quickPress=ID[+ID…]:INDEX`.
            case "menuShow", "quickPress":
                let p = value.split(separator: ":").map(String.init)
                guard p.count >= 2, let window, let content = window.contentView else { break }
                let entries: [MenuEntry]
                if p[0] == "playlist" {
                    guard let row = library.playlist(id: p[1]) else { results.append([name: "no playlist"]); break }
                    entries = PlaylistMenu.entries(row, shell: shell)
                } else {
                    let ids = name == "quickPress" ? p[0] : p[1]
                    let songs = ids.split(separator: "+").compactMap { library.core?.songById(id: String($0)).map { SongRow(core: $0) } }
                    guard !songs.isEmpty else { results.append([name: "no songs"]); break }
                    let remove = p.count > 4 && p[4] == "remove" ? SongMenu.remove(count: songs.count) {} : nil
                    entries = songs.count > 1 ? SongMenu.several(songs, shell: shell, remove: remove) : SongMenu.single(songs[0], shell: shell, remove: remove)
                }
                var menu = makeMenu(entries)
                if p.count > 5, p[5] == "picker", let add = entries.first(where: { $0.quick && $0.submenu != nil }), let rows = add.submenu {
                    menu = makeMenu(rows, find: add.find, empty: add.empty)
                    if p.count > 6 { menu.appearance = NSAppearance(named: p[6] == "light" ? .aqua : .darkAqua) }
                }
                let quick = menu.items.first?.view as? MenuQuickRow
                if name == "quickPress" {
                    quick?.debugPress(Int(p[1]) ?? 0)
                    try? await Task.sleep(for: .milliseconds(1500))
                    results.append(["quickPress": value, "favorites": entries.first.map { $0.checked } ?? false, "toast": shell.toast ?? ""])
                    break
                }
                if p.count > 3, let index = Int(p[3]) { quick?.debugHover(index) }
                nonisolated(unsafe) let open = menu
                let timer = Timer(timeInterval: Double(p[2]) ?? 3, repeats: false) { _ in MainActor.assumeIsolated { open.cancelTracking() } }
                RunLoop.main.add(timer, forMode: .common)
                menu.popUp(positioning: nil, at: NSPoint(x: content.bounds.midX, y: content.bounds.midY), in: content)
                results.append(["menuShow": value, "width": menu.size.width,
                                "quick": quick?.debugButtons.map { "\($0.title) x=\(Int($0.frame.minX)) w=\(Int($0.frame.width))" } ?? [],
                                "quickRowWidth": quick?.bounds.width ?? 0,
                                "rows": menu.items.filter { $0.view == nil }.map { $0.isSeparatorItem ? "—" : ($0.attributedTitle?.string ?? $0.title).trimmingCharacters(in: .whitespaces) + ($0.isEnabled ? "" : " (off)") }])
            // `findSnap=light|dark:TEXT:PATH`: drawn in an off-screen window, so nothing pops up over the user's screen.
            case "findSnap":
                let p = value.split(separator: ":", maxSplits: 2).map(String.init)
                guard p.count == 3 else { break }
                let find = MenuFindField(placeholder: L("Find a playlist"))
                let box = NSView(frame: NSRect(x: 0, y: 0, width: find.frame.width, height: find.frame.height + 10))
                box.wantsLayer = true
                box.layer?.backgroundColor = NSColor(white: p[0] == "light" ? 0.75 : 0.2, alpha: 1).cgColor
                find.frame.origin.y = 5
                box.addSubview(find)
                let off = NSWindow(contentRect: NSRect(x: -20000, y: -20000, width: box.frame.width, height: box.frame.height), styleMask: [.borderless], backing: .buffered, defer: false)
                off.isReleasedWhenClosed = false
                off.appearance = NSAppearance(named: p[0] == "light" ? .aqua : .darkAqua)
                off.contentView = box
                off.orderBack(nil)
                off.makeFirstResponder(find.field)
                find.field.currentEditor()?.insertText(p[1])
                box.layoutSubtreeIfNeeded()
                try? await Task.sleep(for: .milliseconds(300))
                if let rep = box.bitmapImageRepForCachingDisplay(in: box.bounds) {
                    box.cacheDisplay(in: box.bounds, to: rep)
                    try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: p[2]))
                }
                results.append(["findSnap": value, "editor": find.field.currentEditor() != nil])
                off.close()
            // `quickSnap=ID[+ID…]:HOVER:PATH`: drawn off screen, since menus open on top of every window.
            case "quickSnap":
                let p = value.split(separator: ":", maxSplits: 2).map(String.init)
                guard p.count == 3 else { break }
                let songs = p[0].split(separator: "+").compactMap { library.core?.songById(id: String($0)).map { SongRow(core: $0) } }
                guard !songs.isEmpty else { break }
                let menu = makeMenu(songs.count > 1 ? SongMenu.several(songs, shell: shell) : SongMenu.single(songs[0], shell: shell))
                guard let row = menu.items.first?.view as? MenuQuickRow else { break }
                let width = menu.size.width
                let box = NSView(frame: NSRect(x: 0, y: 0, width: width, height: row.bounds.height + 10))
                box.appearance = NSAppearance(named: .darkAqua)
                box.wantsLayer = true
                box.layer?.backgroundColor = NSColor(white: 0.17, alpha: 1).cgColor
                row.frame = NSRect(x: 0, y: 5, width: width, height: row.bounds.height)
                box.addSubview(row)
                if let index = Int(p[1]), index >= 0 { row.debugHover(index) }
                box.layoutSubtreeIfNeeded()
                if let rep = box.bitmapImageRepForCachingDisplay(in: box.bounds) {
                    box.cacheDisplay(in: box.bounds, to: rep)
                    try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: p[2]))
                }
                results.append(["quickSnap": value, "menuWidth": width, "buttons": row.debugButtons.map { "\($0.title) x=\(Int($0.frame.minX)) w=\(Int($0.frame.width))" },
                                "tooltips": row.subviews.map { $0.toolTip ?? "" }])
            // `favoriteOf=ID`: whether that song is a favorite here.
            case "favoriteOf":
                if let row = library.core?.songById(id: value) { results.append(["favoriteOf": value, "on": library.isFavorite(song: SongRow(core: row))]) }
            // `deleteAsk=PLAYLIST_ID`, `deleteConfirm`.
            case "deleteAsk":
                if let row = library.playlist(id: value) { shell.askDeletePlaylist(row) }
                results.append(["deleteAsk": value, "title": shell.confirmation?.title ?? "", "message": shell.confirmation?.message ?? ""])
            case "deleteConfirm":
                guard let request = shell.confirmation else { results.append(["deleteConfirm": "no dialog"]); break }
                shell.dismissConfirmation()
                request.onConfirm()
                try? await Task.sleep(for: .milliseconds(4000))
                results.append(["deleteConfirm": value, "route": "\(shell.route)", "toast": shell.toast ?? "", "stillListed": library.playlists.contains { $0.id == value }, "stored": library.playlist(id: value) != nil])
            // `addTo=PLAYLIST_ID:SONG_ID+SONG_ID`.
            case "addTo":
                let parts = value.split(separator: ":").map(String.init)
                let songs = (parts.count > 1 ? parts[1].split(separator: "+") : []).compactMap { library.core?.songById(id: String($0)).map { SongRow(core: $0) } }
                let result = await library.addToPlaylist(id: parts[0], songs: songs)
                results.append(["addTo": "\(result)", "toast": shell.toast ?? ""])
            // `addSongs=PLAYLIST_ID:SONG_ID+SONG_ID` is the add shared by the menu and drops, which asks about duplicates
            // first. Reports the dialog or the toast.
            case "addSongs":
                let parts = value.split(separator: ":").map(String.init)
                let songs = (parts.count > 1 ? parts[1].split(separator: "+") : []).compactMap { library.core?.songById(id: String($0)).map { SongRow(core: $0) } }
                let name = library.playlists.first { $0.id == parts[0] }?.name ?? ""
                shell.addSongs(songs, toPlaylist: PlaylistRef(id: parts[0], name: name))
                try? await Task.sleep(for: .milliseconds(1500))
                var out: [String: Any] = ["addSongs": value, "toast": shell.toast ?? ""]
                if let d = shell.duplicates { out["dialog"] = ["title": L("Already in {name}", ["name": d.target.name]), "text": DuplicatesLayer.text(d), "fresh": d.fresh.count] }
                results.append(out)
            // `dupAnswer=add|skip|cancel` answers the duplicate warning, then reports the toast and the playlist's songs.
            case "dupAnswer":
                guard let d = shell.duplicates else { results.append(["dupAnswer": "no dialog"]); break }
                if value == "cancel" { shell.closeDuplicates() } else { shell.answerDuplicates(d, skip: value == "skip") }
                try? await Task.sleep(for: .milliseconds(1500))
                let after = await library.storedPlaylistSongs(id: d.target.id) ?? []
                results.append(["dupAnswer": value, "toast": shell.toast ?? "", "songs": after.map(\.title)])
            // `removeFrom=PLAYLIST_ID:PLACE+PLACE` removes songs by position in the stored order.
            case "removeFrom":
                let parts = value.split(separator: ":").map(String.init)
                let stored = await library.storedPlaylistSongs(id: parts[0]) ?? []
                let places = (parts.count > 1 ? parts[1].split(separator: "+") : []).compactMap { Int($0) }
                let result = await library.removeFromPlaylist(id: parts[0], expected: stored.map(\.id), places: places)
                let after = await library.storedPlaylistSongs(id: parts[0]) ?? []
                results.append(["removeFrom": "\(result)", "before": stored.map(\.title), "after": after.map(\.title)])
            // `newPlaylistFor=SONG_ID` makes a new playlist named after the song.
            case "newPlaylistFor":
                if let row = library.core?.songById(id: value).map({ SongRow(core: $0) }) { shell.openNewPlaylist(songs: [row], name: row.title) }
            case "playSong":
                if let row = library.core?.songById(id: value) { player.play([SongRow(core: row)], at: 0, shuffle: false) }
            // `searchPlay=QUERY:N` plays the Nth song of the search results.
            case "searchPlay":
                let parts = value.split(separator: ":", maxSplits: 1).map(String.init)
                let found = await library.searchResults(parts[0], sort: .relevance, favoritesOnly: false)
                let n = parts.count > 1 ? Int(parts[1]) ?? 0 : 0
                if found.songs.indices.contains(n) {
                    results.append(["searchPlay": ["song": found.songs[n].title, "artist": found.songs[n].artist, "results": found.songs.map(\.title)]])
                    shell.playAsRadio(found.songs[n])
                }
            case "addSong":
                if let row = library.core?.songById(id: value) { player.addToQueue(SongRow(core: row)) }
            case "autoRemove": player.removeAutoplay(value.split(separator: "+").compactMap { Int($0) })
            case "autoMove":
                let parts = value.split(separator: ":")
                if parts.count == 2, let before = Int(parts[1]) {
                    let up = player.upcomingCount
                    player.moveInQueue(parts[0].split(separator: "+").compactMap { Int($0) }.map { $0 + up }, before: before, queued: true)
                }
            // `sectionMove=I+J:BEFORE:q|l` moves Up Next rows into Next in Queue (q) or the list (l).
            // `sectionReport` reads the sections, the source and the core's flags.
            case "sectionMove":
                let parts = value.split(separator: ":").map(String.init)
                if parts.count == 3, let before = Int(parts[1]) {
                    player.moveInQueue(parts[0].split(separator: "+").compactMap { Int($0) }, before: before, queued: parts[2] == "q")
                }
            case "clearQueued": shell.clearQueued()
            // `toastUndo` runs the toast's action and reports it.
            case "shuffleQueued": player.shuffleQueued()
            // `shuffleStyle=fewer|standard`.
            case "shuffleStyle": player.shuffleStyle = value == "standard" ? .standard : .fewerRepeats
            case "clearList": shell.clearList()
            // Listen along: `share=on|off`, `listenAlong=USER`, `listenStop`, `listenReport`.
            case "share": player.shareListening = value == "on"
            case "listenAlong": player.listen?.listenAlong(user: value)
            case "listenStop": player.stopFollowing()
            // Spotify friends: `lanyard=ADDRESS`, `friend=ID[:NAME]`, `friendConfirm=ID`, `friendsReport`.
            // Nothing is followed here, since following plays audio.
            case "lanyard":
                player.listen?.lanyardAddress = value
                player.listen?.applyAddress()
            case "friend":
                let parts = value.split(separator: ":", maxSplits: 1).map(String.init)
                player.listen?.addFriend(id: parts[0], name: parts.count > 1 ? parts[1] : "")
            case "friendConfirm": shell.askListenAlongFriend(value)
            case "friendsReport":
                let rows = player.listen?.friends.map { "\($0.name):\($0.reach):\($0.listening ? "\($0.title) by \($0.artist)" : "-"):\($0.song?.id ?? "not in library")" } ?? []
                // Whether each avatar reached the artwork cache at the size the row asks for.
                let avatars = player.listen?.friends.map { f in f.avatarUrl.flatMap(URL.init(string:)).map { ArtworkLoader.shared.cached($0, pixels: Int(T.personArt * 2)) != nil ? "loaded" : "missing" } ?? "none" } ?? []
                results.append(["friendsReport": ["rows": rows, "listed": player.listen?.listeningFriends.map(\.name) ?? [], "avatars": avatars] as [String: Any]])
            // `accountMenu=SECONDS` pops up the account menu for that long, for a capture, and reports where the Private
            // Mode switch ends compared with the other rows' inset.
            case "accountMenu":
                guard let view = window?.contentView else { break }
                let menu = makeMenu(AccountMenu.entries(shell: shell))
                let seconds = Double(value) ?? 3
                // The timer fires on the main run loop while the menu tracks, so the menu stays on the main thread.
                nonisolated(unsafe) let held = menu
                let timer = Timer(timeInterval: seconds, repeats: false) { _ in
                    MainActor.assumeIsolated {
                        let menu = held
                        if let row = menu.items.compactMap({ $0.view as? MenuSwitchRow }).first {
                            let insets = row.debugInsets
                            results.append(["accountMenu": ["switchRightInset": insets.right, "rowWidth": insets.width, "menuWidth": menu.size.width]])
                        }
                        menu.cancelTracking()
                    }
                }
                RunLoop.main.add(timer, forMode: .common)
                menu.popUp(positioning: nil, at: NSPoint(x: view.bounds.maxX - 300, y: view.bounds.maxY - 80), in: view)
            case "listenReport":
                let listed = player.listen?.listing.map { "\($0.user):\($0.song.title):\(String(describing: $0.state))" } ?? []
                results.append(["listenReport": ["active": player.listen?.active ?? false, "listed": listed, "following": player.followingUser ?? "",
                                                 "ended": player.listen?.ended ?? "", "current": player.current?.title ?? "", "waiting": player.waitingSong?.title ?? "",
                                                 "position": player.position, "playing": player.isPlaying] as [String: Any]])
            case "toastUndo":
                results.append(["toast": shell.toast ?? "", "action": shell.toastAction?.label ?? ""])
                shell.runToastAction()
            // `queueRemove=I+J` removes Up Next offsets. `queueRemoveMenu=N` reports the menu item's title for N rows.
            case "queueRemove": player.removeUpcoming(value.split(separator: "+").compactMap { Int($0) })
            case "queueRemoveMenu": results.append(["queueRemoveMenu": SongMenu.removeFromQueue(count: Int(value) ?? 1) {}.title])
            // Get Info: `info=song:ID` (or album, playlist, artist), `infoKey`, `infoClose`. Synced downloads:
            // `toggleDownload=playlist:ID`, `stopDownload=keep|remove`, `meteredSetting=0|1`, `syncedReport`.
            case "info":
                let parts = value.split(separator: ":", maxSplits: 1).map(String.init)
                if parts.count == 2 {
                    switch parts[0] {
                    case "song": shell.openInfo(.song(parts[1]))
                    case "album": shell.openInfo(.album(parts[1]))
                    case "playlist": shell.openInfo(.playlist(parts[1]))
                    default: shell.openInfo(.artist(parts[1]))
                    }
                }
            case "infoKey": shell.infoShortcut()
            case "infoClose": shell.closeInfo()
            case "about": shell.openAbout(value)
            // `discographyReport=<artist id>` reports the artist page's sections once they've loaded.
            case "discographyReport":
                var parts: [DiscographyPart]?
                for _ in 0..<100 {
                    if let artist = library.artist(id: value), let found = library.discography(for: artist) { parts = found; break }
                    try? await Task.sleep(for: .milliseconds(100))
                }
                results.append(["discography": value, "sections": (parts ?? []).map { ["section": $0.section.title, "albums": $0.albums.map { "\($0.name) [\($0.artist)]" }] }, "loaded": parts != nil])
            case "toggleDownload":
                let parts = value.split(separator: ":", maxSplits: 1).map(String.init)
                if parts.count == 2 {
                    let kind: CollectionKind = parts[0] == "album" ? .album : .playlist
                    let name = kind == .album ? library.album(id: parts[1])?.name : library.playlist(id: parts[1])?.name
                    shell.toggleCollectionDownload(kind, id: parts[1], name: name ?? "")
                }
            case "stopDownload": if let request = shell.stopDownload { shell.stopCollectionDownload(request, keepSongs: value == "keep") }
            case "meteredSetting": library.synced?.downloadOnMetered = value == "1"
            case "syncedReport":
                try? await Task.sleep(for: .milliseconds(300))
                results.append(["syncedReport": [
                    "rows": (library.synced?.rows ?? []).map { "\($0.id) \(SyncedDownloads.statusText($0.status)) have=\($0.status.have)/\($0.status.total) bytes=\($0.status.bytes)" },
                    "singles": library.synced?.singleIDs().count ?? -1, "metered": library.synced?.downloadOnMetered ?? false,
                    "dialog": shell.stopDownload.map { StopDownloadLayer.text($0.plan) } ?? "",
                ] as [String: Any]])
            case "sectionReport":
                try? await Task.sleep(for: .milliseconds(300))
                var coreFlags: [String] = []
                var coreSource = ""
                if let snap = player.debugSnapshot, let at = snap.index {
                    coreFlags = snap.queue.dropFirst(Int(at) + 1).prefix(12).map { ($0.queued ? "[q] " : "") + (library.core?.songById(id: $0.id)?.title ?? $0.id) }
                    coreSource = snap.source.map { $0.radio ? "\($0.name) Radio" : $0.name } ?? ""
                }
                results.append(["sectionReport": [
                    "current": player.current?.title ?? "", "source": player.queueSource?.title ?? "", "sourceLinked": player.queueSource?.link != nil,
                    "queued": player.queuedCount, "upNext": (0..<min(player.upcomingCount, 12)).compactMap { player.upcoming(at: $0).map { ($0.queued ? "[q] " : "") + $0.song.title } },
                    "coreAfterCurrent": coreFlags, "coreSource": coreSource,
                ] as [String: Any]])
            case "autoPlay": player.playAutoplay(Int(value) ?? 0)
            case "jumpUp": player.jump(toUpcoming: value == "last" ? player.upcomingCount - 1 : Int(value) ?? 0)
            case "autoplayOn": player.autoplayEnabled = value != "0"
            // `columns=artist+added+plays` sets the visible columns through the menu's toggles.
            case "columns":
                let want = Set(value.split(separator: "+").compactMap { SongColumn(rawValue: String($0)) })
                for column in SongColumn.allCases where want.contains(column) != SongColumn.shown.contains(column) { SongColumn.toggle(column) }
                results.append(["columns": SongColumn.allCases.filter(SongColumn.shown.contains).map(\.rawValue)])
            case "queueVisible": player.setQueueVisible(value != "0")
            case "scroll": scrollMain(to: Double(value) ?? 0)
            // `edgeLog` reports the scroll fade's opacity changes since the last call, as [ms since first, value].
            case "edgeLog":
                let entries = EdgeFadeLog.entries
                EdgeFadeLog.entries.removeAll()
                let start = entries.first?.0 ?? 0
                results.append(["edgeLog": entries.map { [Int(($0.0 - start) * 1000), $0.1] }])
            // `scrollBy=DY:STEPS:MS` scrolls the main view by DY in STEPS, MS apart. `shelfScroll=Y:DX` scrolls the
            // horizontal row at window point (600, Y) sideways by DX.
            case "shelfScroll":
                let v = value.split(separator: ":").compactMap { Double($0) }
                guard v.count == 2, let window, let root = window.contentView?.superview,
                      let hit = root.hitTest(root.convert(toAppKit(CGPoint(x: 600, y: v[0]), window), from: nil)) else { break }
                var view: NSView? = hit
                while let current = view, !(current is NSScrollView && (current as! NSScrollView).documentView.map { $0.frame.width > current.frame.width + 10 } == true) { view = current.superview }
                guard let row = view as? NSScrollView else { results.append(["shelfScroll": "none"]); break }
                let o = row.contentView.bounds.origin
                row.contentView.scroll(to: NSPoint(x: o.x + v[1], y: o.y))
                row.reflectScrolledClipView(row.contentView)
                results.append(["shelfScroll": value, "x": Double(row.contentView.bounds.origin.x)])
            case "scrollBy":
                let v = value.split(separator: ":").compactMap { Double($0) }
                guard v.count == 3, let root = window?.contentView else { break }
                func find(_ view: NSView) -> [NSScrollView] { ((view as? NSScrollView).map { [$0] } ?? []) + view.subviews.flatMap(find) }
                guard let target = find(root).filter({ $0.frame.height > 300 }).max(by: { $0.frame.width * $0.frame.height < $1.frame.width * $1.frame.height }) else { break }
                for _ in 0..<Int(v[1]) {
                    let o = target.contentView.bounds.origin
                    target.contentView.scroll(to: NSPoint(x: o.x, y: o.y + v[0] / v[1]))
                    target.reflectScrolledClipView(target.contentView)
                    try? await Task.sleep(for: .milliseconds(Int(v[2])))
                }
            // `pocketWatch=on|off` records each runloop turn on which the window's scroll pocket views change, and reports
            // on `off`.
            case "pocketWatch":
                if value == "on" {
                    pocketChanges = []
                    var last = ""
                    let start = CFAbsoluteTimeGetCurrent()
                    let observer = CFRunLoopObserverCreateWithHandler(nil, CFRunLoopActivity.beforeWaiting.rawValue, true, 0) { _, _ in
                        MainActor.assumeIsolated {
                            guard let frameView = window?.contentView?.superview else { return }
                            var parts: [String] = []
                            func walk(_ v: NSView) {
                                let name = String(describing: type(of: v))
                                if name.contains("Pocket") || name == "BackdropView" {
                                    parts.append("\(name):\(Int(v.frame.width))x\(Int(v.frame.height)):\(v.isHidden ? "h" : "v"):\(String(format: "%.2f", v.alphaValue))")
                                }
                                v.subviews.forEach(walk)
                            }
                            walk(frameView)
                            let state = parts.joined(separator: " ")
                            if state != last {
                                last = state
                                pocketChanges.append(["ms": Int((CFAbsoluteTimeGetCurrent() - start) * 1000), "state": state])
                            }
                        }
                    }
                    pocketObserver = observer
                    CFRunLoopAddObserver(CFRunLoopGetMain(), observer, .commonModes)
                } else {
                    if let observer = pocketObserver { CFRunLoopRemoveObserver(CFRunLoopGetMain(), observer, .commonModes) }
                    pocketObserver = nil
                    results.append(["pocketChanges": pocketChanges])
                }
            // `edgeViews=NAME` lists every effect, material and edge view in the window with its frame, alpha and visibility.
            case "edgeViews":
                guard let frameView = window?.contentView?.superview else { break }
                var found: [[String: Any]] = []
                func walk(_ v: NSView, depth: Int) {
                    let name = String(describing: type(of: v))
                    if ["Edge", "Pocket", "Effect", "Material", "Backdrop", "Glass"].contains(where: name.contains) {
                        let f = v.convert(v.bounds, to: nil)
                        found.append(["class": name, "x": Int(f.minX), "y": Int(f.minY), "w": Int(f.width), "h": Int(f.height),
                                      "alpha": Double(v.alphaValue), "hidden": v.isHidden || v.isHiddenOrHasHiddenAncestor, "depth": depth])
                    }
                    for sub in v.subviews { walk(sub, depth: depth + 1) }
                }
                walk(frameView, depth: 0)
                results.append(["edgeViews": value, "views": found])
            // `rowProbe=X:Y0:Y1:STEP[:0]` walks down the song table at X, reporting the view and row a click reaches and
            // whether sent clicks and moves get through. A trailing 0 sends no clicks.
            case "rowProbe":
                let v = value.split(separator: ":").compactMap { Double($0) }
                guard v.count >= 4, let window, let content = window.contentView, let root = content.superview,
                      let scroll = findTable(content), let table = scroll.documentView as? SongsNSTableView else { results.append(["rowProbe": "no table"]); break }
                var samples: [[String: Any]] = []
                var y = v[1]
                while y <= v[2] {
                    let p = toAppKit(CGPoint(x: v[0], y: y), window)
                    let hit = root.hitTest(root.convert(p, from: nil))
                    var chain: [String] = []
                    var h = hit
                    while let c = h, chain.count < 5 { chain.append(String(describing: type(of: c))); h = c.superview }
                    let row = table.row(at: table.convert(p, from: nil))
                    if let move = NSEvent.mouseEvent(with: .mouseMoved, location: p, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 0, pressure: 0) {
                        window.sendEvent(move)
                    }
                    let hovered = table.hoveredRow
                    let before = SongsNSTableView.debugMouseDowns
                    if v.count < 5 || v[4] != 0 { await sendClick(at: CGPoint(x: v[0], y: y)) }
                    samples.append(["y": y, "hit": chain.joined(separator: "<"), "row": row, "hover": hovered, "down": SongsNSTableView.debugMouseDowns > before])
                    y += v[3]
                }
                let titlebar = root.subviews.first { String(describing: type(of: $0)).contains("Titlebar") }
                results.append(["rowProbe": value, "key": window.isKeyWindow, "scrollY": scroll.contentView.bounds.origin.y, "insetTop": scroll.contentInsets.top,
                                "titlebarH": titlebar.map { Double($0.frame.height) } ?? -1, "layoutTop": Double(content.bounds.height - window.contentLayoutRect.maxY),
                                "samples": samples])
            // `layerWatch=on|off` records each runloop turn on which a titlebar, toolbar, pocket, backdrop or material layer
            // changes its visibility, opacity, size or animations.
            case "layerWatch":
                if value == "on" {
                    layerChanges = []
                    var last: [String: String] = [:]
                    let start = CFAbsoluteTimeGetCurrent()
                    let observer = CFRunLoopObserverCreateWithHandler(nil, CFRunLoopActivity.beforeWaiting.rawValue, true, 0) { _, _ in
                        MainActor.assumeIsolated {
                            guard let frameLayer = window?.contentView?.superview?.layer else { return }
                            let now = edgeLayerStates(frameLayer)
                            var changes: [String] = []
                            for (key, state) in now where last[key] != state { changes.append("\(key) \(last[key] ?? "new") -> \(state)") }
                            for key in last.keys where now[key] == nil { changes.append("\(key) gone") }
                            last = now
                            if !changes.isEmpty, layerChanges.count < 3000 {
                                layerChanges.append(["ms": Int((CFAbsoluteTimeGetCurrent() - start) * 1000), "changes": changes.sorted()])
                            }
                        }
                    }
                    layerObserver = observer
                    CFRunLoopAddObserver(CFRunLoopGetMain(), observer, .commonModes)
                } else {
                    if let observer = layerObserver { CFRunLoopRemoveObserver(CFRunLoopGetMain(), observer, .commonModes) }
                    layerObserver = nil
                    results.append(["layerChanges": layerChanges])
                }
            // `wheel=X:Y:DY:STEPS:MS` sends a trackpad-style scroll to the view at that point, so the scroll view takes its
            // live-scroll path.
            case "wheel":
                let v = value.split(separator: ":").compactMap { Double($0) }
                // A sixth value of 1 sends the events through the window instead.
                guard v.count >= 5, let window, let root = window.contentView?.superview else { break }
                let point = toAppKit(CGPoint(x: v[0], y: v[1]), window)
                guard let target = root.hitTest(root.convert(point, from: nil)) else { break }
                let viaWindow = v.count > 5 && v[5] == 1
                let clip = target.enclosingScrollView?.contentView
                var seen: [Double] = []
                let steps = max(1, Int(v[3]))
                for i in 0...(steps + 1) {
                    let phase: Int64 = i == 0 ? 1 : i == steps + 1 ? 4 : 2
                    let delta = (i == 0 || i == steps + 1) ? 0 : Int32(-v[2] / Double(steps))
                    guard let cg = CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 1, wheel1: delta, wheel2: 0, wheel3: 0) else { continue }
                    cg.setIntegerValueField(.scrollWheelEventScrollPhase, value: phase)
                    cg.setIntegerValueField(.scrollWheelEventIsContinuous, value: 1)
                    if viaWindow {
                        cg.location = CGPoint(x: window.frame.minX + point.x, y: (NSScreen.screens.first?.frame.height ?? 0) - (window.frame.minY + point.y))
                        cg.setIntegerValueField(.mouseEventWindowUnderMousePointer, value: Int64(window.windowNumber))
                        if let event = NSEvent(cgEvent: cg) { window.sendEvent(event) }
                    } else if let event = NSEvent(cgEvent: cg) { target.scrollWheel(with: event) }
                    try? await Task.sleep(for: .milliseconds(Int(v[4])))
                    if let clip { seen.append(Double(clip.bounds.origin.y)) }
                }
                try? await Task.sleep(for: .milliseconds(900))
                results.append(["wheel": value, "offsets": seen, "settled": clip.map { Double($0.bounds.origin.y) } ?? 0])
            // `tableState` reports the song table's hovered and selected rows, mouse downs, key status and the pointer.
            case "tableState":
                guard let window, let content = window.contentView, let scroll = findTable(content), let table = scroll.documentView as? SongsNSTableView else { results.append(["tableState": "no table"]); break }
                let m = window.mouseLocationOutsideOfEventStream
                results.append(["hovered": table.hoveredRow, "selected": Array(table.selectedRowIndexes), "downs": SongsNSTableView.debugMouseDowns,
                                "key": window.isKeyWindow, "mouse": [Int(m.x), Int(content.bounds.height - m.y)], "scrollY": scroll.contentView.bounds.origin.y])
            case "route": applyRoute(value, shell: shell)
            case "tableGeom": if let window { results.append(TableCheck.geometry(window)) }
            case "account": if let window { results.append(await AccountCheck.run(value, shell: shell, window: window)) }
            case "tableDragProbe": if let window { results.append(TableCheck.dragProbe(window)) }
            case "headerClick": if let window { results.append(await TableCheck.headerClick(value, window: window)) }
            case "linkClick": if let window { results.append(await TableCheck.linkClick(value, window: window) { "\(shell.route)" }) }
            // `hitClick=X:Y` sends a click straight to the view the window's hit test finds, then reports the route, the
            // table's selection and header, and the view's class.
            case "hitClick":
                let v = value.split(separator: ":").compactMap { Double($0) }
                guard v.count == 2, let window, let root = window.contentView?.superview else { break }
                let point = toAppKit(CGPoint(x: v[0], y: v[1]), window)
                let hit = root.hitTest(root.convert(point, from: nil))
                if let hit, let down = NSEvent.mouseEvent(with: .leftMouseDown, location: point, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                                                          windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1) {
                    hit.mouseDown(with: down)
                }
                try? await Task.sleep(for: .milliseconds(500))
                var out = TableCheck.dragProbe(window)
                out["hitClick"] = value
                out["hitView"] = hit.map { String(describing: type(of: $0)) } ?? "none"
                out["route"] = "\(shell.route)"
                results.append(out)
            case "tableDrop": if let window { results.append(await TableCheck.drop(value, window: window)) }
            // `titleDoubleClick=X:Y` sends a double-click through the window, then reports what the title bar responder did.
            case "titleDoubleClick":
                let v = value.split(separator: ":").compactMap { Double($0) }
                guard v.count == 2, let window else { break }
                TitlebarDoubleClick.last = ""
                TitlebarDoubleClick.performed = 0
                let before = (window.isZoomed, window.frame)
                let point = toAppKit(CGPoint(x: v[0], y: v[1]), window)
                for (type, count) in [(NSEvent.EventType.leftMouseDown, 1), (.leftMouseUp, 1), (.leftMouseDown, 2), (.leftMouseUp, 2)] {
                    if let e = NSEvent.mouseEvent(with: type, location: point, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                                                  windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: count, pressure: type == .leftMouseDown ? 1 : 0) {
                        // Through the app, so its event monitors see the clicks like real ones.
                        NSApp.sendEvent(e)
                    }
                    try? await Task.sleep(for: .milliseconds(40))
                }
                let root = window.contentView?.superview
                let hit = root?.hitTest(root!.convert(point, from: nil))
                try? await Task.sleep(for: .milliseconds(700))
                results.append(["zoomed": [before.0, window.isZoomed], "width": [Double(before.1.width), Double(window.frame.width)], "performed": TitlebarDoubleClick.performed,
                                "titleDoubleClick": value, "did": TitlebarDoubleClick.last, "setting": TitlebarDoubleClick.action, "hit": hit.map { String(describing: type(of: $0)) } ?? "none",
                                "chain": window.contentView?.nextResponder.map { String(describing: type(of: $0)) } ?? "none", "route": "\(shell.route)"])
            // `tableGapDrag=I+J:ROW[:end]` drags songs I and J over table row ROW so the gap opens, then reports the frames.
            // `end` drops them.
            case "tableGapDrag": if let window { results.append(await TableCheck.gapDrag(value, window: window)) }
            case "tableDragCard": if let w = window ?? mainWindow { results.append(TableCheck.dragCard(value, window: w)) }
            case "songDrop": if let window { results.append(await SongDropCheck.run(value, shell: shell, window: window)) }
            case "songDragCard": if let window { results.append(SongDropCheck.card(value, shell: shell, window: window)) }
            // `hoverLive=KEY|1` hovers a control by its HoverDebug key (0 leaves), and `pressLive` presses it.
            // `hoverSamples` returns the scale frames recorded since the last call.
            case "hoverLive", "pressLive":
                let parts = value.split(separator: "|").map(String.init)
                if parts.count == 2 {
                    HoverLive.samples.removeAll()
                    if name == "hoverLive" {
                        if parts[1] == "1" { HoverLive.shared.hovered.insert(parts[0]) } else { HoverLive.shared.hovered.remove(parts[0]) }
                    } else {
                        if parts[1] == "1" { HoverLive.shared.pressed.insert(parts[0]) } else { HoverLive.shared.pressed.remove(parts[0]) }
                    }
                }
            // `clickLog`: real clicks since the last call, and when each grow probe first moved.
            case "clickLog":
                let first = Dictionary(HoverLive.samples.map { ($0.key, $0.time) }, uniquingKeysWith: min)
                results.append(["clickLog": clicks, "firstSample": first])
                clicks.removeAll()
                HoverLive.samples.removeAll()
            case "hoverSamples":
                let all = HoverLive.samples
                let start = all.first?.time ?? 0
                results.append(["hoverSamples": value, "frames": all.filter { value.isEmpty || $0.key == value }.map { [($0.time - start) * 1000, $0.scale] }])
                HoverLive.samples.removeAll()
            case "orderSaves": results.append(["orderSaves": PlaylistOrderDebug.results])
            case "playlistSort": PlaylistOrderDebug.songs?.sort = SongSort(rawValue: value) ?? .order
            case "playlistQuery": PlaylistOrderDebug.songs?.query = value
            case "genreQuery": GenresDebug.setQuery?(value)
            case "filter": FiltersDebug.apply(value)
            case "filterClear": FiltersDebug.clear(value)
            case "genreReport":
                let page = library.genrePage
                results.append(["step": "genreReport", "top": page.top.map { "\($0.name):\($0.albums):\($0.covers.count)" }, "rows": page.all.count,
                                "rowsWithCover": page.all.filter { !$0.covers.isEmpty }.count, "first": page.all.prefix(12).map(\.name),
                                "browse": page.browse.prefix(12).map { "\($0.name):\($0.albums):\($0.covers.count)" }, "browseCount": page.browse.count,
                                "topCoversUnique": Set(page.top.flatMap(\.covers)).count == page.top.flatMap(\.covers).count, "tinted": GenresDebug.tinted.sorted()])
            // `queueDrag=PRESS:ROWS:DY1+DY2+...:drop|esc` lifts Up Next row PRESS with ROWS, moves the pointer down by each
            // DY, then drops or cancels.
            case "queueDrag":
                let v = value.split(separator: ":").map(String.init)
                guard v.count == 4, let drag = QueueDragDebug.current, let tracker = drag.tracker, let press = Int(v[0]) else { results.append(["queueDrag": "no queue"]); break }
                let rows = v[1].split(separator: "+").compactMap { Int($0) }
                let start = tracker.convert(NSPoint(x: 40, y: drag.rowTop(press) + drag.rowHeight / 2), to: nil)
                drag.lift(row: press, rows: rows, at: start)
                var seen: [[String: Any]] = []
                for dy in v[2].split(separator: "+").compactMap({ Double($0) }) {
                    drag.follow(NSPoint(x: start.x, y: start.y - dy))
                    try? await Task.sleep(for: .milliseconds(60))
                    seen.append(["dy": dy, "before": drag.before ?? -1, "slotY": Double(drag.slot?.minY ?? -1), "slotH": Double(drag.slot?.height ?? 0),
                                 "shifts": (0..<min(drag.count, 8)).map { Double(drag.shift($0)) }, "follow": Double(drag.followShift), "offset": Double(drag.liftedOffset),
                                 "intoFirst": drag.intoFirst, "splitShift": Double(drag.splitShift)])
                }
                // `hold` leaves it lifted for a capture. `queueDragEnd=drop|esc` ends it.
                if v[3] != "hold" { drag.finish(drop: v[3] == "drop") }
                results.append(["queueDrag": seen])
            case "queueDragEnd": QueueDragDebug.current?.finish(drop: value == "drop")
            case "tableClick": if let window { results.append(await TableCheck.click(value, window: window) { player.current?.title ?? "" }) }
            case "tableMove": if let window { results.append(await TableCheck.move(value, window: window)) }
            case "tableReveal": if let window { results.append(TableCheck.reveal(value, window: window)) }
            case "tableWheel": if let window { results.append(await TableCheck.wheel(value, window: window)) }
            case "tableTop": if let window { results.append(TableCheck.top(window)) }
            case "tableMakeKey": if let window { results.append(TableCheck.makeKey(window)) }
            case "tableSelectAll": if let window { results.append(TableCheck.selectAll(window)) }
            case "tableKey": if let window { results.append(await TableCheck.key(value, window: window)) }
            case "dragProbe": if let window { results.append(TopBandCheck.dragProbe(value, window: window)) }
            case "layerDump": if let window { results.append(TopBandCheck.layerDump(value, window: window)) }
            case "eventWatch": if let window, let r = TopBandCheck.eventWatch(value, window: window) { results.append(r) }
            case "postClick": if let window { results.append(await TopBandCheck.postClick(value, window: window) { "\(shell.route)" }) }
            // `focus=other|main|resign` gives key status to a small window of our own or back to the main one, or
            // deactivates the app.
            case "focus":
                guard let main = window ?? mainWindow else { break }
                switch value {
                case "other":
                    let other = otherWindow ?? NSWindow(contentRect: NSRect(x: main.frame.minX + 40, y: main.frame.minY + 40, width: 200, height: 120), styleMask: [.titled], backing: .buffered, defer: false)
                    other.isReleasedWhenClosed = false
                    otherWindow = other
                    other.makeKeyAndOrderFront(nil)
                case "main": otherWindow?.orderOut(nil); main.makeKeyAndOrderFront(nil)
                case "resign": NSApp.deactivate()
                default: break
                }
                results.append(["focus": value, "key": main.isKeyWindow, "active": NSApp.isActive])
            // `routeFrames=<route>` opens the route and samples the song table for half a second, to catch the header drawn
            // over the rows in the first frames.
            case "routeFrames":
                // Each runloop turn just before it sleeps, after that turn's drawing was committed.
                var samples: [[String: Any]] = []
                let start = CFAbsoluteTimeGetCurrent()
                let observer = CFRunLoopObserverCreateWithHandler(nil, CFRunLoopActivity.beforeWaiting.rawValue, true, 0) { _, _ in
                    MainActor.assumeIsolated {
                        let ms = Int((CFAbsoluteTimeGetCurrent() - start) * 1000)
                        if let root = window?.contentView, let scroll = findTable(root) {
                            let table = scroll.documentView as? NSTableView
                            samples.append(["ms": ms, "table": String(UInt(bitPattern: ObjectIdentifier(scroll).hashValue) % 100000), "alpha": scroll.alphaValue,
                                            "insetTop": scroll.contentInsets.top, "scrollY": scroll.contentView.bounds.origin.y, "rows": table?.numberOfRows ?? 0])
                        } else {
                            samples.append(["ms": ms, "table": "none"])
                        }
                    }
                }
                CFRunLoopAddObserver(CFRunLoopGetMain(), observer, .commonModes)
                applyRoute(value, shell: shell)
                try? await Task.sleep(for: .milliseconds(900))
                CFRunLoopRemoveObserver(CFRunLoopGetMain(), observer, .commonModes)
                results.append(["routeFrames": value, "samples": samples])
            // `sidebarMove=from:gap` in your own group, or `sidebarMove=shared:from:gap`.
            case "sidebarMove":
                let shared = value.hasPrefix("shared:")
                let v = value.split(separator: ":").compactMap { Int($0) }
                let account = library.accountKey, saved = SidebarOrder.load(account: account)
                let group: (PlaylistSection) -> [String] = { section in SidebarOrder.merged(saved: saved, current: library.playlists.filter { $0.section == section }).map(\.id) }
                var ids = group(shared ? .shared : .mine)
                if v.count == 2, SidebarOrder.move(&ids, from: v[0], toGap: v[1]) { SidebarOrder.save(ids + group(shared ? .mine : .shared), account: account) }
                results.append(["sidebarOrder": Array(ids.prefix(5))])
            // The sidebar's two groups in order, the public mark, and what each Playlists scope holds.
            case "sharedReport":
                let saved = SidebarOrder.load(account: library.accountKey)
                let group: (PlaylistSection) -> [String] = { section in SidebarOrder.merged(saved: saved, current: library.playlists.filter { $0.section == section }).map(\.name) }
                let lists = library.playlistsList()
                try? await Task.sleep(for: .milliseconds(600))
                var scopes: [String: [String]] = [:]
                for scope in PlaylistScope.allCases {
                    lists.scope = scope
                    scopes[scope.rawValue] = (0..<lists.count).compactMap { lists.playlist(at: $0).map { "\($0.name)\($0.isMine ? "" : " / " + playlistSubtitle($0))" } }
                }
                lists.scope = .all
                results.append(["mine": group(.mine), "shared": group(.shared), "marked": library.playlists.filter { $0.section == .mine && $0.isPublic }.map(\.name),
                                "hasOthers": lists.hasOthers, "scopes": scopes])
            // `scrobbleSet=0|1` sets Scrobble plays. `scrobbleReport` reads it back.
            case "scrobbleSet": shell.chooseScrobble(value == "1")
            case "scrobbleReport":
                let choice = UserDefaults.standard.object(forKey: AccountPrefs.key("scrobbleChoice")) as? Bool
                results.append(["scrobblePlays": shell.scrobblePlays, "choice": choice.map { $0 ? "on" : "off" } ?? "none", "testRun": CoreMode.isTestRun])
            case "playlistCovers":
                results.append(["playlistCovers": library.playlists.map { ["id": $0.id, "name": $0.name, "smart": $0.isSmart, "server": $0.coverURL != nil, "grid": $0.collage.count] as [String: Any] }])
            // `playlistTint=<id>` reports the cover, the tint read from it and the page's neutral state.
            case "playlistTint":
                if let row = library.playlist(id: value) {
                    let source = PlaylistCoverSource(row)
                    let tint = await PlaylistTints.shared.read(source)
                    results.append(["playlistTint": value, "name": row.name, "source": "\(source)", "tint": tint ?? "neutral"])
                }
            case "createPlaylist":
                do { results.append(["created": try await library.createPlaylist(name: value, songs: []) ?? "(no id)"]) } catch { results.append(["createFailed": "\(error)"]) }
            // `playlistId=NAME`.
            case "playlistId": results.append(["playlistId": library.playlists.filter { $0.name == value }.map(\.id), "route": "\(shell.route)"])
            // `coverStyle=PLAYLIST_ID:STYLE_ID:TITLE`.
            case "coverStyle":
                let v = value.split(separator: ":", maxSplits: 2).map(String.init)
                if v.count == 3, let style = CoverPicker.styles.first(where: { $0.id == v[1] }), let file = await writeCoverImage(style: style, title: v[2]) {
                    let bytes = (try? Data(contentsOf: file))?.count ?? 0
                    results.append(["coverStyle": await library.setPlaylistImage(id: v[0], path: file.path) ?? "ok", "bytes": bytes])
                    try? FileManager.default.removeItem(at: file)
                } else { results.append(["coverStyle": "not drawn"]) }
            // `newPlaylistDialog[=SLOT[:NAME]]`.
            case "newPlaylistDialog":
                let v = value.split(separator: ":", maxSplits: 1).map(String.init)
                shell.coverSlotSeed = v.first.flatMap { Int($0) }
                if v.count == 2 { shell.openNewPlaylist(songs: [], name: v[1]) } else { shell.openNewPlaylist() }
            // `editPlaylistDialog=PLAYLIST_ID[:SLOT]`.
            case "editPlaylistDialog":
                let v = value.split(separator: ":", maxSplits: 1).map(String.init)
                shell.coverSlotSeed = v.count == 2 ? Int(v[1]) : nil
                if let id = v.first, let playlist = library.playlist(id: id) {
                    shell.closeEditPlaylist()
                    // A frame apart, so a dialog already open is rebuilt on the new slot.
                    try? await Task.sleep(for: .milliseconds(100))
                    shell.openEditPlaylist(playlist)
                    results.append(["editPlaylistDialog": playlist.name, "owned": playlist.isOwned])
                } else { results.append(["editPlaylistDialog": "no such playlist"]) }
            // `selectPopup=X:Y`.
            case "selectPopup":
                let v = value.split(separator: ":").compactMap { Double($0) }
                let options = ["Automatic", "Lossless", "High", "Normal", "Low"].map { SelectOption(value: $0, label: L($0)) }
                shell.popups.open(PopupCenter.Request(label: "", value: "Lossless", options: options,
                                                      anchor: CGRect(x: v.first ?? 600, y: v.last ?? 300, width: 240, height: T.controlH), choose: { _ in }))
            // `coverUpload=<playlist id>|<file>` and `coverRemove=<playlist id>`. Refused unless the launch names that
            // playlist in NAVIA_TEST_COVER_PLAYLIST.
            case "coverUpload":
                let v = value.split(separator: "|", maxSplits: 1).map(String.init)
                if v.count == 2 { results.append(["coverUpload": await library.setPlaylistImage(id: v[0], path: v[1]) ?? "ok"]) }
            case "coverRemove": results.append(["coverRemove": await library.removePlaylistImage(id: value) ?? "ok"])
            case "coverSupport": results.append(["canChangePlaylistImages": library.canChangePlaylistImages])
            // `lyricsDump=FROM:TO`: start, end, word count and text of each lyric row, gap rows included.
            case "lyricsDump":
                let v = value.split(separator: ":").compactMap { Int($0) }
                let lines = player.lyrics.withGaps()
                let upper = min(lines.count, (v.last ?? lines.count - 1) + 1)
                let range = min(v.first ?? 0, upper)..<upper
                results.append(["lyricsDump": range.map { i in "\(i) \(lines[i].start ?? -1) end \(lines[i].endMs ?? -1) words \(lines[i].words.count) \(lines[i].text)" },
                                "rows": lines.count])
            // `lyricBuilds`.
            case "lyricBuilds": results.append(["lyricBuilds": LyricFillView.builds, "alive": LyricFillView.census()]); LyricFillView.builds = []
            // `lyricOrder`: reports where the line shown goes backwards, skips, or never shows.
            case "lyricOrder":
                let lines = player.lyrics.withGaps()
                guard let last = lines.last?.start else { results.append(["lyricOrder": "no synced lyrics"]); break }
                var shown = Set<Int>(), problems: [String] = [], previous = 0
                for ms in stride(from: 0, through: last + 10_000, by: 10) {
                    let index = lines.shownIndex(atMs: ms)
                    if index < previous { problems.append("back \(previous)->\(index) at \(ms)") }
                    if index > previous + 1 { problems.append("skip \(previous)->\(index) at \(ms)") }
                    previous = index
                    shown.insert(index)
                }
                let never = lines.indices.filter { !shown.contains($0) }
                results.append(["lyricOrder": problems.isEmpty && never.isEmpty ? "ok" : "problems", "problems": Array(problems.prefix(20)), "neverShown": never])
            // `dialogSubmit`.
            case "dialogSubmit": shell.dialogSubmit += 1
            case "wait": try? await Task.sleep(for: .milliseconds(Int(value) ?? 0))
            case "syncReport":
                let p = library.syncProgress
                results.append(["syncReport": value, "state": "\(library.state)", "shown": p != nil, "stage": p?.stage.map { "\($0)" } ?? "none",
                                "done": Int(p?.done ?? 0), "total": p?.total.map { Int($0) } ?? -1, "fraction": p?.fraction.map { Double($0) } ?? -1, "first": p?.first ?? false,
                                "detail": p.flatMap(syncDetail) ?? "", "onPage": library.syncMeterOnPage])
            // `click=X:Y` and `drag=X1:Y1:X2:Y2` take window points from the top left and go to this window only.
            case "click":
                let xy = value.split(separator: ":").compactMap { Double($0) }
                if xy.count == 2 { await sendClick(at: CGPoint(x: xy[0], y: xy[1])) }
            // `hit=X:Y` reports which view a click at that point reaches.
            case "hit":
                let xy = value.split(separator: ":").compactMap { Double($0) }
                if xy.count == 2, let window, let root = window.contentView?.superview {
                    let point = toAppKit(CGPoint(x: xy[0], y: xy[1]), window)
                    let view = root.hitTest(root.convert(point, from: nil))
                    var chain: [String] = []
                    var v = view
                    while let current = v, chain.count < 6 { chain.append(String(describing: type(of: current))); v = current.superview }
                    results.append(["hit": value, "chain": chain])
                }
            case "drag":
                let v = value.split(separator: ":").compactMap { Double($0) }
                if v.count == 4 { await sendDrag(from: CGPoint(x: v[0], y: v[1]), to: CGPoint(x: v[2], y: v[3])) }
            case "snap": snap(value, path: path)
            case "station":
                if let station = library.stations.first {
                    player.playStation(station)
                    results.append(["station": station.name])
                } else { results.append(["station": "none"]) }
            case "stationStop": player.stopStation()
            case "downloadFirst":
                let songs = [player.current].compactMap { $0 }
                library.setDownloaded(songs: songs, true)
                results.append(["downloadRequested": songs.map(\.title)])
            // `quality=streaming:mp3:192`, `quality=metered:original:0`, `quality=download:aac:128`.
            case "quality":
                let v = value.split(separator: ":").map(String.init)
                if v.count == 3, let q = library.quality {
                    let slot: QualitySettings.Slot = v[0] == "metered" ? .metered : v[0] == "download" ? .download : .streaming
                    q.set(slot, format: v[1], kbps: UInt32(v[2]) ?? 0)
                }
            case "leveling": library.quality?.setLeveling(value)
            // `playThreshold=75`, `playThresholdCap=0|1`, `playThresholdReport`.
            case "playThreshold": if let p = UInt32(value) { library.playThreshold?.setPercent(p) }
            case "playThresholdCap": library.playThreshold?.setCap(value == "1")
            case "playThresholdReport":
                try? await Task.sleep(for: .milliseconds(200))
                let shown: PlayThresholdRow? = library.playThreshold?.current
                let saved: PlayThresholdRow? = library.core?.playThreshold()
                let choices: [Int] = (library.playThreshold?.choices ?? []).map { Int($0) }
                results.append(["percent": Int(shown?.percent ?? 0), "cap": shown?.capAtFourMinutes ?? false,
                                "corePercent": Int(saved?.percent ?? 0), "coreCap": saved?.capAtFourMinutes ?? false,
                                "choices": choices, "scrobblePlays": shell.scrobblePlays, "privateMode": player.privateMode])
            case "privateMode": player.privateMode = value == "1"
            case "qualityReport":
                if let q = library.quality {
                    results.append(["formats": q.formats.map(\.id), "streaming": "\(q.streaming.format) \(q.streaming.kbps)", "metered": "\(q.metered.format) \(q.metered.kbps)", "download": "\(q.download.format) \(q.download.kbps)", "leveling": q.leveling, "connectionMetered": q.connectionMetered, "quality": CoreCheck.describe(player.quality)])
                }
            // Song cache: `storageReport`, `storageLimit=<bytes>`, `storageConfirm`, `storageClear`.
            case "storageReport":
                if let storage = library.storage {
                    storage.refresh()
                    try? await Task.sleep(for: .milliseconds(800))
                    let u = storage.usage
                    let current = player.currentSong?.id ?? ""
                    results.append(["songCacheBytes": u?.songCacheBytes ?? 0, "cachedSongs": u?.cachedSongs ?? 0, "limit": storage.limit, "lowDisk": u?.songCacheLowDisk ?? false,
                                    "artworkBytes": u?.artworkBytes ?? 0, "lyricsBytes": u?.lyricsBytes ?? 0, "downloadsBytes": u?.downloadsBytes ?? 0,
                                    "current": current, "currentCached": library.core?.isSongCached(id: current) ?? false, "quality": CoreCheck.describe(player.quality)])
                }
            case "storageLimit": library.storage?.setLimit(UInt64(value) ?? 0)
            case "storageConfirm":
                shell.confirm(ConfirmRequest(title: L("Clear the cache?"), message: L("Cached songs, artwork and lyrics are removed from this device. Downloads and your library stay, and anything removed comes from the server again when it's needed."),
                                             confirmTitle: L("Clear Cache"), compact: true) { library.storage?.clear() })
            case "storageClear": library.storage?.clear()
            case "report": report(library, player, value.isEmpty ? name : value, extra: [:], path, shell: shell)
            case "visibility": setVisibility(value)
            // `artAlias=KEY:COVER`; empty clears.
            case "artAlias":
                let v = value.split(separator: ":", maxSplits: 1).map(String.init)
                if v.count == 2 { CoreArt.alias[v[0]] = v[1] } else { CoreArt.alias.removeAll() }
                ArtworkLoader.shared.clear()
            case "animReport": results.append(["animReport": AnimatedArtProbe.report()])
            case "reduceMotion":
                Motion.shared.force(value == "system" ? nil : value == "on")
                results.append(["reduceMotion": Motion.isReduced])
            case "motionSample": if let window { MotionCheck.start(value, window: window) }
            case "motionReport": results.append(MotionCheck.report(value))
            // `motionSeries=NAME:PROBE`.
            case "motionSeries":
                let parts = value.split(separator: ":").map(String.init)
                if parts.count == 2 { results.append(["motionSeries": value, "values": MotionCheck.series(parts[0], parts[1])]) }
            // `windowShown=0|1` holds until the next occlusion change, so a check can run on a locked screen.
            case "windowShown": shell.setWindowShown(value != "0")
            case "expandAnimated": MotionCheck.expand(shell, value != "0")
            case "visReport": visibilityReport(shell, player, name: value)
            // `watchWindow` logs every notification about the main window or the app as `windowEvents`.
            case "watchWindow": watchWindow()
            case "windowEvents": results.append(["windowEvents": windowEvents]); windowEvents = []
            case "wav":
                if let frames = try? player.player?.saveCapture(path: value) { results.append(["wavFrames": frames, "wavPath": value]) }
            case "quit":
                write(path, ["results": results])
                NSApp.terminate(nil)
            default: results.append(["unknownStep": step])
            }
    }

    private static func connectReport(_ shell: ShellModel) {
        guard let form = shell.connect else { results.append(["connect": "closed", "signedIn": shell.activeLogin?.host ?? ""]); return }
        results.append(["connect": "open", "mode": "\(form.mode)", "address": form.address, "user": form.user, "insecure": form.isInsecure, "canSubmit": form.canSubmit, "deezerStep": form.deezerStep, "rejected": form.rejected, "outcome": "\(form.outcome.map { "\($0)" } ?? "nil")"])
    }

    /// The song table's scroll view under `view`.
    private static func findTable(_ view: NSView) -> NSScrollView? {
        if let scroll = view as? NSScrollView, scroll.documentView is NSTableView { return scroll }
        for sub in view.subviews { if let found = findTable(sub) { return found } }
        return nil
    }

    private static func recordWindow() {
        guard let window, let screen = window.screen ?? NSScreen.main else { return }
        let f = window.frame
        results.append(["windowNumber": window.windowNumber, "x": f.minX, "y": screen.frame.maxY - f.maxY, "width": f.width, "height": f.height])
    }

    /// Scrolls the page's main scroll view, so edge effects can be captured.
    private static func scrollMain(to y: Double) {
        guard let root = NSApp.windows.first(where: { $0.isVisible && $0.contentView != nil })?.contentView else { return }
        func find(_ view: NSView) -> [NSScrollView] { ((view as? NSScrollView).map { [$0] } ?? []) + view.subviews.flatMap(find) }
        let scrolls = find(root).filter { $0.frame.height > 300 }
        guard let target = scrolls.max(by: { $0.frame.width * $0.frame.height < $1.frame.width * $1.frame.height }) else { return }
        // `NSClipView.scroll(to:)` doesn't clamp like a real wheel, so an oversized value would show blank space.
        let top = target.contentInsets.top
        let maxY = max(0, (target.documentView?.frame.height ?? 0) - target.contentView.bounds.height)
        target.contentView.scroll(to: NSPoint(x: 0, y: min(y, maxY) - top))
        target.reflectScrolledClipView(target.contentView)
        results.append(["scrollTarget": y, "scrollMax": Double(maxY), "documentHeight": Double(target.documentView?.frame.height ?? 0), "clipHeight": Double(target.contentView.bounds.height), "insetTop": Double(target.contentInsets.top)])
    }

    private static var window: NSWindow? {
        if let found = NSApp.windows.filter({ $0.isVisible && $0.contentView != nil && $0 !== coverWindow }).max(by: { $0.frame.width < $1.frame.width }) {
            mainWindow = found
            return found
        }
        return nil
    }
    /// The main window, kept so it can be found while minimized or hidden.
    private static weak var mainWindow: NSWindow?
    /// An opaque window of our own that covers the main one without touching other apps' windows.
    private static var coverWindow: NSWindow?
    /// A small window of our own that takes key status from the main one.
    private static var otherWindow: NSWindow?

    /// `visibility=mini|demini|hide|unhide|cover|uncover|front` hides or shows the window without real input.
    private static func setVisibility(_ how: String) {
        guard let main = window ?? mainWindow else { results.append(["visibility": how, "error": "no window"]); return }
        switch how {
        case "mini": main.miniaturize(nil)
        case "demini": main.deminiaturize(nil)
        case "hide": NSApp.hide(nil)
        case "unhide": NSApp.unhideWithoutActivation()
        // Above other apps' windows without taking focus, for a window opened behind them with `open -g`.
        case "front": main.orderFrontRegardless()
        case "cover":
            let cover = coverWindow ?? NSWindow(contentRect: main.frame.insetBy(dx: -40, dy: -40), styleMask: [.borderless], backing: .buffered, defer: false)
            cover.isReleasedWhenClosed = false
            cover.isOpaque = true
            cover.backgroundColor = .black
            cover.hasShadow = false
            // Needs drawn content, or the window server may not count it as covering anything.
            let fill = NSView()
            fill.wantsLayer = true
            fill.layer?.backgroundColor = NSColor.black.cgColor
            fill.layer?.isOpaque = true
            cover.contentView = fill
            cover.setFrame(main.frame.insetBy(dx: -40, dy: -40), display: true)
            cover.orderFrontRegardless()
            cover.display()
            coverWindow = cover
        case "uncover": coverWindow?.orderOut(nil); coverWindow = nil
        default: break
        }
        results.append(["visibility": how])
    }

    private static var windowEvents: [String] = []
    private static var windowWatchers: [NSObjectProtocol] = []
    private static func watchWindow() {
        guard let main = window ?? mainWindow, windowWatchers.isEmpty else { return }
        let log: (Notification) -> Void = { note in
            MainActor.assumeIsolated {
                guard windowEvents.count < 400, !note.name.rawValue.contains("Update"), !note.name.rawValue.contains("Layout") else { return }
                windowEvents.append(String(format: "%.3f %@ mini=%d occl=%d", CFAbsoluteTimeGetCurrent(), note.name.rawValue,
                                           main.isMiniaturized ? 1 : 0, main.occlusionState.contains(.visible) ? 1 : 0))
            }
        }
        windowWatchers.append(NotificationCenter.default.addObserver(forName: nil, object: main, queue: .main, using: log))
        windowWatchers.append(NotificationCenter.default.addObserver(forName: nil, object: NSApp, queue: .main, using: log))
    }

    /// `visReport` reports the window state and the visual counters.
    private static func visibilityReport(_ shell: ShellModel, _ player: CorePlayerModel, name: String) {
        let main = window ?? mainWindow
        results.append(["visReport": name, "t": CFAbsoluteTimeGetCurrent(),
                        "occlusionVisible": main.map { $0.occlusionState.contains(.visible) } ?? false,
                        "miniaturized": main?.isMiniaturized ?? false, "appHidden": NSApp.isHidden, "appActive": NSApp.isActive,
                        "shellShown": shell.windowShown,
                        "position": player.position, "heardPosition": player.heardPosition, "playing": player.isPlaying,
                        "lyricFrames": VisualStats.lyricFrames, "lyricsBodies": VisualStats.lyricsBodies,
                        "barBodies": VisualStats.barBodies, "lyricGlides": VisualStats.lyricGlides, "lyricJumps": VisualStats.lyricJumps, "writers": VisualStats.writers])
    }

    private static func toAppKit(_ p: CGPoint, _ window: NSWindow) -> NSPoint {
        NSPoint(x: p.x, y: (window.contentView?.bounds.height ?? 0) - p.y)
    }

    private static func mouseEvent(_ type: NSEvent.EventType, at point: NSPoint, window: NSWindow, count: Int = 1) -> NSEvent? {
        NSEvent.mouseEvent(with: type, location: point, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: count, pressure: 1)
    }

    private static func sendClick(at p: CGPoint) async {
        guard let window else { return }
        let point = toAppKit(p, window)
        guard let down = mouseEvent(.leftMouseDown, at: point, window: window), let up = mouseEvent(.leftMouseUp, at: point, window: window) else { return }
        NSApp.postEvent(up, atStart: false)
        window.sendEvent(down)
        try? await Task.sleep(for: .milliseconds(200))
    }

    private static func sendDrag(from a: CGPoint, to b: CGPoint) async {
        guard let window else { return }
        let start = toAppKit(a, window), end = toAppKit(b, window)
        guard let down = mouseEvent(.leftMouseDown, at: start, window: window) else { return }
        window.sendEvent(down)
        for i in 1...12 {
            let t = CGFloat(i) / 12
            if let e = mouseEvent(.leftMouseDragged, at: NSPoint(x: start.x + (end.x - start.x) * t, y: start.y + (end.y - start.y) * t), window: window) { window.sendEvent(e) }
            try? await Task.sleep(for: .milliseconds(40))
        }
        if let up = mouseEvent(.leftMouseUp, at: end, window: window) { window.sendEvent(up) }
        try? await Task.sleep(for: .milliseconds(300))
    }

    /// A screenshot of this window only, next to the report.
    private static func snap(_ name: String, path: String) {
        guard let window else { return }
        let dir = (path as NSString).deletingLastPathComponent
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        process.arguments = ["-x", "-l", "\(window.windowNumber)", "\(dir)/\(name).png"]
        try? process.run()
        process.waitUntilExit()
    }

    private static func applyRoute(_ name: String, shell: ShellModel) {
        shell.go(Route(startName: name))
    }

    private static func report(_ library: CoreLibraryModel, _ player: CorePlayerModel, _ name: String, extra: [String: Any], _ path: String, shell: ShellModel) {
        var entry: [String: Any] = ["name": name]
        entry.merge(extra) { a, _ in a }
        entry["timeline"] = AppTimeline.snapshot
        entry["residentMB"] = Double(residentMemoryBytes()) / 1_048_576
        entry["songs"] = library.allSongs.count
        entry["eq"] = player.debugEqDescription
        entry["station"] = player.station?.name ?? ""
        entry["downloaded"] = library.collection.downloadedSongs.count
        entry["connected"] = library.connected.map { $0 ? 1 : 0 } ?? -1
        entry["offline"] = library.isOffline
        entry["state"] = "\(library.state)"
        entry["route"] = "\(shell.route)"
        entry["toast"] = shell.toast ?? ""
        entry["waiting"] = player.isWaitingForServer
        entry["volumeNow"] = player.volume
        entry["volumeSaved"] = UserDefaults.standard.object(forKey: "volume") as? Double ?? -1
        entry["muted"] = player.isMuted
        entry["outputDevice"] = shell.outputDevice ?? ""
        entry["outputs"] = shell.outputOptions.map(\.name)
        if let current = player.current {
            entry["title"] = current.title
            entry["position"] = player.position
            entry["isPlaying"] = player.isPlaying
            entry["queueUpcoming"] = player.upcomingCount
            entry["shuffle"] = player.shuffle
            entry["repeat"] = player.repeatMode.rawValue
            entry["lyrics"] = player.lyrics.count
        }
        if let snap = player.debugSnapshot {
            let coreState: [String: Any] = ["position": snap.positionMs, "volume": snap.volume, "rate": snap.outputRate, "device": snap.device ?? "", "underrun": snap.underrunFrames, "rebuffer": snap.rebufferFrames, "xruns": snap.xruns, "status": "\(snap.status)", "queue": snap.queue.count]
            entry["core"] = coreState
        }
        entry["nowPlaying"] = player.nowPlayingSnapshot
        results.append(entry)
    }

    private static func write(_ path: String, _ object: [String: Any]) {
        var object = object
        // One value JSON can't hold would otherwise stop every later write.
        if let list = object["results"] as? [Any] {
            object["results"] = list.map { JSONSerialization.isValidJSONObject($0) ? $0 : ["unwritable": String(describing: $0)] }
        }
        guard let data = try? JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted, .sortedKeys]) else { return }
        try? data.write(to: URL(fileURLWithPath: path))
    }
}

/// Counts of visual work for `visReport`, such as lyric frames and whether lyrics glided or jumped to a new line.
@MainActor enum VisualStats {
    static var lyricFrames = 0
    static var lyricsBodies = 0
    static var barBodies = 0
    static var lyricGlides = 0
    static var lyricJumps = 0
    static var writers = ""
    /// `RowDragSlot` bodies so far and the slot's top in the last one (-1: none), for `songDrop` reports.
    static var dragSlot: [String: Double] = [:]
}
#endif
