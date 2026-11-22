#if DEBUG
import AppKit

/// A stand-in dragging session over the table for `tableGapDrag`: rows of this table moving, held at a point.
@MainActor final class GapDragInfo: NSObject, NSDraggingInfo {
    private let window: NSWindow
    private let location: NSPoint
    private let source: NSTableView
    private let rows: [Int]
    private let table: NSTableView
    private let pasteboard = NSPasteboard(name: NSPasteboard.Name("dev.navia.gapcheck"))

    init(window: NSWindow, location: NSPoint, source: NSTableView, rows: [Int], table: NSTableView) {
        self.window = window
        self.location = location
        self.source = source
        self.rows = rows
        self.table = table
        super.init()
        pasteboard.clearContents()
        pasteboard.writeObjects(rows.map { r in let item = NSPasteboardItem(); item.setString(String(r), forType: .naviaSongRow); return item })
    }

    var draggingDestinationWindow: NSWindow? { window }
    var draggingSourceOperationMask: NSDragOperation { .move }
    var draggingLocation: NSPoint { location }
    var draggedImageLocation: NSPoint { location }
    var draggedImage: NSImage? { nil }
    var draggingPasteboard: NSPasteboard { pasteboard }
    var draggingSource: Any? { source }
    var draggingSequenceNumber: Int { 4242 }
    func slideDraggedImage(to screenPoint: NSPoint) {}
    var draggingFormation: NSDraggingFormation = .none
    var animatesToDestination = false
    var numberOfValidItemsForDrop: Int = 1
    func enumerateDraggingItems(options enumOpts: NSDraggingItemEnumerationOptions = [], for view: NSView?, classes classArray: [AnyClass],
                                searchOptions: [NSPasteboard.ReadingOptionKey: Any] = [:], using block: (NSDraggingItem, Int, UnsafeMutablePointer<ObjCBool>) -> Void) {
        var stop: ObjCBool = false
        for (i, r) in rows.enumerated() {
            let item = NSDraggingItem(pasteboardWriter: NSPasteboardItem())
            item.draggingFrame = table.rect(ofRow: r)
            block(item, i, &stop)
            if stop.boolValue { break }
        }
    }
    var springLoadingHighlight: NSSpringLoadingHighlight { .none }
    func resetSpringLoading() {}
}

/// CoreCheck steps for clicks, hover and keys on the song table, sent as real events so NSTableView's own
/// handling runs. Points are window points from the top left.
@MainActor enum TableCheck {
    static var scrollRowCalls = 0

    /// The table on screen. SwiftUI can keep an old page's table around hidden or off to the side, so the visible one
    /// with the most room wins.
    static func table(_ window: NSWindow) -> SongsNSTableView? {
        func all(_ v: NSView) -> [SongsNSTableView] { ((v as? SongsNSTableView).map { [$0] } ?? []) + v.subviews.flatMap(all) }
        let tables = window.contentView.map(all) ?? []
        return tables.filter { !$0.isHiddenOrHasHiddenAncestor && $0.window != nil }
            .max { $0.visibleRect.width * $0.visibleRect.height < $1.visibleRect.width * $1.visibleRect.height } ?? tables.first
    }

    private static func appKit(_ x: Double, _ y: Double, _ window: NSWindow) -> NSPoint {
        NSPoint(x: x, y: (window.contentView?.bounds.height ?? 0) - y)
    }

    /// A rect in window points from the top left, as [minY, maxY], plus x and width when `full`.
    private static func span(_ r: NSRect, from view: NSView, _ window: NSWindow, full: Bool = false) -> [Double] {
        let w = view.convert(r, to: nil)
        let h = window.contentView?.bounds.height ?? 0
        let ys = [Double(h - w.maxY), Double(h - w.minY)]
        return full ? [Double(w.minX), Double(w.width)] + ys : ys
    }

    private static func mouse(_ type: NSEvent.EventType, _ p: NSPoint, _ window: NSWindow, clicks: Int = 1) -> NSEvent? {
        NSEvent.mouseEvent(with: type, location: p, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                           windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: clicks, pressure: type == .mouseMoved ? 0 : 1)
    }

    /// `tableDragProbe`: the drag writer for each row (headers and the bottom room must give none), where proposed drops
    /// land, whether reordering is on, the emphasized column, and the songs.
    static func dragProbe(_ window: NSWindow) -> [String: Any] {
        guard let table = table(window), let c = table.controller else { return ["tableDragProbe": "no table"] }
        let n = table.numberOfRows
        let writers = (0..<min(n, 8)).map { c.tableView(table, pasteboardWriterForRow: $0) != nil } + [c.tableView(table, pasteboardWriterForRow: n - 1) != nil]
        let drops = [0, 1, c.leadRows, c.leadRows + 1, n - 1, n + 3].map { c.dropRow(proposed: $0) ?? -1 }
        let titles = (0..<min(c.songs?.count ?? 0, 8)).map { c.songs?.song(at: $0)?.title ?? "" }
        return ["tableDragProbe": true, "reorder": c.reorder != nil, "rows": n, "leadRows": c.leadRows, "writers": writers, "dropRows": drops,
                "registered": table.registeredDraggedTypes.map(\.rawValue), "titles": titles, "selected": table.selectedRowIndexes.map { $0 },
                "links": (0..<table.numberOfRows).lazy.compactMap { table.view(atColumn: 0, row: $0, makeIfNecessary: false) as? SongCell }.first.map { $0.debugLinkSpans() } ?? [],
                "headerY": table.numberOfRows > 1 ? span(table.rect(ofRow: 1), from: table, window) : [],
                "sort": c.songs.map { "\($0.sort.rawValue)\($0.descending ? ":desc" : "")" } ?? "", "rowY": rowYs(table, window)]
    }

    /// Where the first rows sit (row, minY, maxY), and the link spans of the first song.
    private static func rowYs(_ table: SongsNSTableView, _ window: NSWindow) -> [[Double]] {
        (0..<min(table.numberOfRows, 4)).map { r in [Double(r)] + span(table.rect(ofRow: r), from: table, window) }
    }

    /// `headerClick=COL`: a plain click on column header COL at its label, sent to what the window's hit test finds
    /// there, which must be the header. Then the probe.
    static func headerClick(_ value: String, window: NSWindow) async -> [String: Any] {
        guard let col = Int(value), let table = table(window), table.numberOfRows > 1,
              let header = table.view(atColumn: 0, row: 1, makeIfNecessary: false) as? SongHeaderView else { return ["headerClick": "no header"] }
        let label = header.labelFrame(col)
        let point = header.convert(NSPoint(x: label.minX + 6, y: label.midY), to: nil)
        guard let root = window.contentView?.superview, let hit = root.hitTest(root.convert(point, from: nil)) else { return ["headerClick": "no hit"] }
        var out: [String: Any] = ["headerClick": col, "hitView": String(describing: type(of: hit))]
        // Only the header. A press sent to the table would wait in its tracking loop for a release that never comes.
        if hit === header, let down = event(.leftMouseDown, point, window) { hit.mouseDown(with: down) }
        try? await Task.sleep(for: .milliseconds(500))
        out.merge(dragProbe(window)) { a, _ in a }
        return out
    }

    /// `linkClick=I[:cmd]`: a click on the first song row's link I through the table's own mouseDown, only if the table
    /// finds a link there. With `cmd`, Command is held.
    static func linkClick(_ value: String, window: NSWindow, route: () -> String) async -> [String: Any] {
        let parts = value.split(separator: ":")
        guard let i = parts.first.flatMap({ Int($0) }), let table = table(window),
              let cell = (0..<table.numberOfRows).lazy.compactMap({ table.view(atColumn: 0, row: $0, makeIfNecessary: false) as? SongCell }).first else { return ["linkClick": "no row"] }
        let spans = cell.debugLinkSpans()
        guard spans.indices.contains(i), let x0 = spans[i]["x0"] as? Double, let x1 = spans[i]["x1"] as? Double, let y = spans[i]["y"] as? Double else { return ["linkClick": "no link"] }
        let point = appKit((x0 + x1) / 2, y, window)
        let flags: NSEvent.ModifierFlags = parts.count > 1 && parts[1] == "cmd" ? [.command] : []
        let found = table.link(at: table.convert(point, from: nil)) != nil
        let before = route()
        if found, flags.isEmpty, let down = event(.leftMouseDown, point, window) { table.mouseDown(with: down) }
        try? await Task.sleep(for: .milliseconds(600))
        return ["linkClick": i, "found": found, "target": spans[i]["route"] ?? "", "routeBefore": before, "routeAfter": route(),
                "selected": table.selectedRowIndexes.map { $0 }]
    }

    private static func event(_ type: NSEvent.EventType, _ p: NSPoint, _ window: NSWindow) -> NSEvent? {
        NSEvent.mouseEvent(with: type, location: p, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                           windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1)
    }

    /// `tableGapDrag=I+J:ROW[:end|:hold]`: a drag of songs I and J held over table row ROW, through the table's drag
    /// destination methods, so NSTableView opens its gap as for a real drag. Reports the slot and the row frames.
    /// `hold` leaves it open until `tableGapDrag=end`. `end` drops it.
    private static var gapInfo: GapDragInfo?
    static func gapDrag(_ value: String, window: NSWindow) async -> [String: Any] {
        guard let table = table(window), let c = table.controller else { return ["tableGapDrag": "no table"] }
        let parts = value.split(separator: ":").map(String.init)
        if parts.first == "end" || parts.first == "exit" {
            guard let info = gapInfo else { return ["tableGapDrag": "none open"] }
            let drop = parts.first == "end"
            // Called from the run loop, as AppKit would. The drop's animation spins a nested run loop, and a task running
            // there fails Swift's executor checks.
            outsideTask {
                if drop, table.prepareForDragOperation(info) { _ = table.performDragOperation(info) } else { table.draggingExited(info) }
                c.debugEndDrag()
            }
            gapInfo = nil
            try? await Task.sleep(for: .milliseconds(400))
            var out = dragProbe(window)
            out["tableGapDrag"] = value
            return out
        }
        guard parts.count >= 2, let row = Int(parts[1]) else { return ["tableGapDrag": "bad"] }
        let songs = parts[0].split(separator: "+").compactMap { Int($0) }
        c.debugBeginDrag(songs: songs)
        let target = table.rect(ofRow: min(row, table.numberOfRows - 1))
        let point = table.convert(NSPoint(x: table.visibleRect.midX, y: target.minY + 4), to: nil)
        let info = GapDragInfo(window: window, location: point, source: table, rows: songs.map { c.tableRow($0) }, table: table)
        gapInfo = info
        outsideTask { _ = table.draggingEntered(info) }
        for _ in 0..<6 {
            try? await Task.sleep(for: .milliseconds(60))
            outsideTask { _ = table.draggingUpdated(info) }
        }
        try? await Task.sleep(for: .milliseconds(60))
        try? await Task.sleep(for: .milliseconds(400))
        var out: [String: Any] = ["tableGapDrag": value, "gap": c.debugGap()]
        if parts.count < 3 || parts[2] != "hold" {
            outsideTask {
                table.draggingExited(info)
                c.debugEndDrag()
            }
            gapInfo = nil
            try? await Task.sleep(for: .milliseconds(400))
            out["after"] = c.debugGap()
        }
        return out
    }

    private static func outsideTask(_ work: @escaping @MainActor () -> Void) {
        DispatchQueue.main.async { MainActor.assumeIsolated { work() } }
    }

    /// `tableDragCard=ROW:COUNT:FILE`: writes the drag image for COUNT rows pressed on row ROW as a PNG.
    static func dragCard(_ value: String, window: NSWindow) -> [String: Any] {
        let parts = value.split(separator: ":", maxSplits: 2).map(String.init)
        guard parts.count == 3, let row = Int(parts[0]), let count = Int(parts[1]), let table = table(window), let c = table.controller,
              let card = c.dragCard(row: row, count: count), let tiff = card.image.tiffRepresentation,
              let png = NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]) else { return ["tableDragCard": "none"] }
        try? png.write(to: URL(fileURLWithPath: parts[2]))
        return ["tableDragCard": value, "frame": [Double(card.frame.minX), Double(card.frame.minY), Double(card.frame.width), Double(card.frame.height)]]
    }

    /// `tableDrop=I+J:ROW`, through the same path as `acceptDrop`.
    static func drop(_ value: String, window: NSWindow) async -> [String: Any] {
        let parts = value.split(separator: ":")
        guard parts.count == 2, let row = Int(parts[1]), let table = table(window), let c = table.controller else { return ["tableDrop": "bad"] }
        let songs = parts[0].split(separator: "+").compactMap { Int($0) }
        let moved = c.drop(songs: songs, aboveRow: row)
        // Right after the drop, before the server answers, the page already shows the new order.
        let now = (0..<min(c.songs?.count ?? 0, 8)).map { c.songs?.song(at: $0)?.title ?? "" }
        try? await Task.sleep(for: .milliseconds(400))
        var out = dragProbe(window)
        out["tableDrop"] = moved
        out["titlesNow"] = now
        return out
    }

    /// `tableGeom`.
    static func geometry(_ window: NSWindow) -> [String: Any] {
        guard let table = table(window), let scroll = table.enclosingScrollView else { return ["tableGeom": "no table"] }
        let clip = scroll.contentView
        let areas = table.trackingAreas.map { a -> [String: Any] in
            let rect = a.options.contains(.inVisibleRect) ? table.visibleRect : a.rect
            return ["inVisibleRect": a.options.contains(.inVisibleRect), "span": span(rect, from: table, window)]
        }
        return ["tableGeom": true, "scrollView": span(scroll.bounds, from: scroll, window), "clipFrame": span(clip.bounds, from: clip, window),
                "clipOriginY": Double(clip.bounds.origin.y), "insetTop": Double(scroll.contentInsets.top), "insetBottom": Double(scroll.contentInsets.bottom),
                "tableFrame": [Double(table.frame.minY), Double(table.frame.height)], "tableBoundsY": Double(table.bounds.origin.y),
                "visibleRect": span(table.visibleRect, from: table, window), "documentVisibleRect": span(scroll.documentVisibleRect, from: clip, window), "trackingAreas": areas,
                "visibleRows": [table.rows(in: table.visibleRect).location, table.rows(in: table.visibleRect).length],
                // Whether the rows can show: the faintest alpha from the table up, and how many row views are laid out.
                "minAlpha": Double(sequence(first: table as NSView, next: { $0.superview }).map(\.alphaValue).min() ?? 1),
                "hiddenAncestor": table.isHiddenOrHasHiddenAncestor,
                "rowViews": { var n = 0; table.enumerateAvailableRowViews { _, _ in n += 1 }; return n }(),
                "numberOfRows": table.numberOfRows, "tableWidth": Double(table.bounds.width)]
    }

    /// `tableClick=X:Y[:2][:direct]`: a click or double-click at the point, through the window or, with `direct`,
    /// straight to the table's `mouseDown`. The table ignores a first click while the app is inactive, and the check
    /// must not take focus. Reports the scroll offset, the row under the point, the selection and what's playing.
    static func click(_ value: String, window: NSWindow, playing: () -> String) async -> [String: Any] {
        let v = value.split(separator: ":").compactMap { Double($0) }
        let direct = value.hasSuffix(":direct")
        guard v.count >= 2, let table = table(window), let clip = table.enclosingScrollView?.contentView else { return ["tableClick": "bad"] }
        let p = appKit(v[0], v[1], window)
        let count = v.count > 2 ? Int(v[2]) : 1
        let before = Double(clip.bounds.origin.y), rowBefore = table.row(at: table.convert(p, from: nil))
        let calls = scrollRowCalls
        for n in 1...max(1, count) {
            guard let down = mouse(.leftMouseDown, p, window, clicks: n), let up = mouse(.leftMouseUp, p, window, clicks: n) else { break }
            NSApp.postEvent(up, atStart: false)
            if direct { table.mouseDown(with: down) } else { window.sendEvent(down) }
            try? await Task.sleep(for: .milliseconds(n < count ? 60 : 400))
        }
        let after = Double(clip.bounds.origin.y)
        return ["tableClick": value, "scrollBefore": before, "scrollAfter": after, "rowAtPoint": rowBefore,
                "rowAtPointAfter": table.row(at: table.convert(p, from: nil)), "selected": Array(table.selectedRowIndexes), "selectedSongs": table.selectedRowIndexes.compactMap { table.controller?.songIndex($0) },
                "scrollRowToVisibleCalls": scrollRowCalls - calls, "playing": playing(), "key": window.isKeyWindow,
                "firstResponderIsTable": window.firstResponder === table]
    }

    /// `tableReveal=X:Y`: scrolls the row under the point into view, as a click does. A visible row should not move.
    static func reveal(_ value: String, window: NSWindow) -> [String: Any] {
        let v = value.split(separator: ":").compactMap { Double($0) }
        guard v.count >= 2, let table = table(window), let clip = table.enclosingScrollView?.contentView else { return ["tableReveal": "bad"] }
        let row = table.row(at: table.convert(appKit(v[0], v[1], window), from: nil))
        let before = Double(clip.bounds.origin.y)
        table.scrollRowToVisible(row)
        return ["tableReveal": value, "row": row, "scrollBefore": before, "scrollAfter": Double(clip.bounds.origin.y),
                "rowSpan": row >= 0 ? span(table.rect(ofRow: row), from: table, window) : []]
    }

    /// `tableWheel=DY:STEPS:MS:MODE[:MOMENTUM]`: a trackpad gesture on the scroll view, then MOMENTUM decaying momentum
    /// events. Positive DY scrolls down. MODE: `view` calls `scrollWheel`, `window` sends through the window, `post`
    /// queues on the app, `mixed` gives the first to the scroll view and queues the rest. Reports the offsets.
    static func wheel(_ value: String, window: NSWindow) async -> [String: Any] {
        let parts = value.split(separator: ":").map(String.init)
        guard parts.count >= 4, let dy = Double(parts[0]), let steps = Int(parts[1]), let ms = Int(parts[2]),
              let table = table(window), let scroll = table.enclosingScrollView else { return ["tableWheel": "bad"] }
        let mode = parts[3], momentum = parts.count > 4 ? Int(parts[4]) ?? 0 : 0
        let clip = scroll.contentView
        let center = scroll.convert(NSPoint(x: scroll.bounds.midX, y: scroll.bounds.midY), to: nil)
        let screenH = NSScreen.screens.first?.frame.height ?? 0
        let global = CGPoint(x: window.frame.minX + center.x, y: screenH - (window.frame.minY + center.y))
        func make(_ delta: Double, phase: Int64, momentum m: Int64) -> NSEvent? {
            guard let cg = CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 1, wheel1: Int32(-delta.rounded()), wheel2: 0, wheel3: 0) else { return nil }
            cg.setIntegerValueField(.scrollWheelEventScrollPhase, value: phase)
            cg.setIntegerValueField(.scrollWheelEventMomentumPhase, value: m)
            cg.setIntegerValueField(.scrollWheelEventIsContinuous, value: 1)
            cg.setDoubleValueField(.scrollWheelEventPointDeltaAxis1, value: -delta)
            cg.setDoubleValueField(.scrollWheelEventFixedPtDeltaAxis1, value: -delta)
            cg.location = global
            cg.setIntegerValueField(.mouseEventWindowUnderMousePointer, value: Int64(window.windowNumber))
            cg.setIntegerValueField(.mouseEventWindowUnderMousePointerThatCanHandleThisEvent, value: Int64(window.windowNumber))
            return NSEvent(cgEvent: cg)
        }
        var events: [NSEvent] = []
        let per = dy / Double(max(1, steps))
        if mode == "plain" {
            // No gesture phases: precise deltas that NSScrollView applies one by one on the main thread.
            for _ in 0..<max(1, steps) { if let e = make(per, phase: 0, momentum: 0) { events.append(e) } }
        } else {
        if let e = make(0, phase: 1, momentum: 0) { events.append(e) }
        for _ in 0..<max(1, steps) { if let e = make(per, phase: 2, momentum: 0) { events.append(e) } }
        if let e = make(0, phase: 4, momentum: 0) { events.append(e) }
        var m = per
        for i in 0..<momentum {
            m *= 0.88
            if let e = make(m, phase: 0, momentum: i == 0 ? 1 : 2) { events.append(e) }
        }
        if momentum > 0, let e = make(0, phase: 0, momentum: 3) { events.append(e) }
        }
        var offsets: [Double] = []
        var windowed = 0
        for (i, e) in events.enumerated() {
            if e.window === window { windowed += 1 }
            switch i == 0 && mode == "mixed" || mode == "plain" ? "view" : mode {
            case "mixed": NSApp.postEvent(e, atStart: false)
            case "window": window.sendEvent(e)
            case "post": NSApp.postEvent(e, atStart: false)
            default: scroll.scrollWheel(with: e)
            }
            try? await Task.sleep(for: .milliseconds(ms))
            offsets.append(Double(clip.bounds.origin.y))
        }
        try? await Task.sleep(for: .milliseconds(1500))
        return ["tableWheel": value, "events": events.count, "eventsWithWindow": windowed, "min": offsets.min() ?? 0, "max": offsets.max() ?? 0,
                "settled": Double(clip.bounds.origin.y), "insetTop": Double(scroll.contentInsets.top), "offsets": offsets.map { ($0 * 10).rounded() / 10 }]
    }

    /// `tableTop`: scrolls to the top as the page does on open.
    static func top(_ window: NSWindow) -> [String: Any] {
        guard let table = table(window), let scroll = table.enclosingScrollView else { return ["tableTop": "no table"] }
        scroll.contentView.scroll(to: NSPoint(x: 0, y: -scroll.contentInsets.top))
        scroll.reflectScrolledClipView(scroll.contentView)
        return ["tableTop": Double(scroll.contentView.bounds.origin.y), "insetTop": Double(scroll.contentInsets.top)]
    }

    /// `tableMakeKey`: makes the window key inside this app only, without activating it, which NSTableView needs before
    /// it acts on a click.
    static func makeKey(_ window: NSWindow) -> [String: Any] {
        window.makeKey()
        if let table = table(window) { window.makeFirstResponder(table) }
        return ["tableMakeKey": window.isKeyWindow]
    }

    /// `tableSelectAll`.
    static func selectAll(_ window: NSWindow) -> [String: Any] {
        guard let table = table(window), let c = table.controller else { return ["tableSelectAll": "no table"] }
        table.selectAll(nil)
        let rows = table.selectedRowIndexes
        let songs = rows.compactMap { c.songIndex($0) }
        let result: [String: Any] = ["tableSelectAll": rows.count, "songs": songs.count, "lead": c.leadRows, "first": rows.first ?? -1, "last": rows.last ?? -1, "tableRows": table.numberOfRows]
        table.deselectAll(nil)
        return result
    }

    /// `tableMove=X:Y[:app|direct]`: a mouse move at the point through the window, the app, or straight to
    /// `mouseMoved`. Reports the hovered row against the row under it.
    static func move(_ value: String, window: NSWindow) async -> [String: Any] {
        let parts = value.split(separator: ":")
        let v = parts.prefix(2).compactMap { Double($0) }
        let via = parts.count > 2 ? String(parts[2]) : "window"
        guard v.count == 2, let table = table(window), let e = mouse(.mouseMoved, appKit(v[0], v[1], window), window) else { return ["tableMove": "bad"] }
        let before = table.hoveredRow
        switch via {
        case "app": NSApp.sendEvent(e)
        case "direct": table.mouseMoved(with: e)
        default: window.sendEvent(e)
        }
        try? await Task.sleep(for: .milliseconds(80))
        return ["tableMove": value, "hoverBefore": before, "hovered": table.hoveredRow,
                "rowAtPoint": table.row(at: table.convert(appKit(v[0], v[1], window), from: nil)), "key": window.isKeyWindow]
    }

    /// `tableKey=down|up|N`: arrow keys to the table. Reports the selection, the scroll offset and where the selected
    /// row sits.
    static func key(_ value: String, window: NSWindow) async -> [String: Any] {
        guard let table = table(window), let clip = table.enclosingScrollView?.contentView else { return ["tableKey": "no table"] }
        let up = value == "up"
        let presses = Int(value) ?? 1
        let code: UInt16 = up ? 126 : 125
        let char = String(Character(UnicodeScalar(up ? 0xF700 : 0xF701)!))
        let before = Double(clip.bounds.origin.y)
        for _ in 0..<presses {
            if let e = NSEvent.keyEvent(with: .keyDown, location: .zero, modifierFlags: [.numericPad, .function], timestamp: ProcessInfo.processInfo.systemUptime,
                                        windowNumber: window.windowNumber, context: nil, characters: char, charactersIgnoringModifiers: char, isARepeat: false, keyCode: code) {
                window.sendEvent(e)
            }
            try? await Task.sleep(for: .milliseconds(60))
        }
        try? await Task.sleep(for: .milliseconds(200))
        let row = table.selectedRow
        return ["tableKey": value, "scrollBefore": before, "scrollAfter": Double(clip.bounds.origin.y), "selected": row,
                "rowSpan": row >= 0 ? span(table.rect(ofRow: row), from: table, window) : [], "firstResponderIsTable": window.firstResponder === table]
    }
}
#endif
