import AppKit
import SwiftUI

/// Press-and-hold reordering for a SwiftUI list of equal-height rows (the queue). The lifted rows follow the pointer
/// as a card, the other rows part where they'd land, and the list scrolls near its edges. Release drops, Esc
/// restores. Positions are row offsets.
@MainActor @Observable final class RowDrag {
    static var slide: Animation? { T.easeOut(T.dragSlide) }

    let rowHeight: CGFloat
    private(set) var rows: [Int] = []
    /// The row under the pointer when it lifted, or nil when nothing is lifted.
    private(set) var pressed: Int?
    /// in the list's own coordinates, top down.
    private(set) var pointerY: CGFloat = 0
    /// The rows land before this row.
    private(set) var before: Int?
    @ObservationIgnored private var grab: CGFloat = 0
    @ObservationIgnored var count = 0
    /// Rows from this one on (the queue's Autoplay songs) can be lifted but not dropped among. They sit `limitGap`
    /// lower.
    @ObservationIgnored var limit: Int?
    @ObservationIgnored var limitGap: CGFloat = 0
    /// Where the second queue section starts, with a heading `splitGap` tall above it. The heading decides which
    /// section a drop joins.
    @ObservationIgnored var split: Int?
    @ObservationIgnored var splitGap: CGFloat = 0
    /// Whether the drop joins the first section (Next in Queue).
    private(set) var intoFirst = false
    @ObservationIgnored var defaultIntoFirst = false
    @ObservationIgnored weak var tracker: NSView?
    /// Called with the rows, the row they land before and whether they join the first section, when a drop changes
    /// the order or the section.
    @ObservationIgnored var onMove: ([Int], Int, Bool) -> Void = { _, _, _ in }
    /// How many songs from another list are over this list. The rows part for them as for a move.
    private(set) var incoming: Int?
    /// With nothing queued, the Next from heading sits above the rows, this tall. Songs carried in above its middle go
    /// into Next in Queue.
    @ObservationIgnored var headAbove: CGFloat = 0
    /// Called when the pointer leaves the list with rows lifted. The drag carries on as songs dragged out.
    @ObservationIgnored var onLeave: ((NSEvent, [Int]) -> Void)?
    @ObservationIgnored private var monitor: Any?
    @ObservationIgnored private var timer: Timer?
    @ObservationIgnored private var windowPoint: NSPoint?

    init(rowHeight: CGFloat) { self.rowHeight = rowHeight }

    var isActive: Bool { pressed != nil }

    var liftedOffset: CGFloat {
        guard let pressed else { return 0 }
        return pointerY - grab - rowTop(pressed)
    }

    func rowTop(_ row: Int) -> CGFloat {
        CGFloat(row) * rowHeight + (limit.map { row >= $0 } == true ? limitGap : 0) + (split.map { row >= $0 } == true ? splitGap : 0)
    }

    func isCarried(_ row: Int) -> Bool { pressed != nil && pressed != row && rows.contains(row) }

    /// Where the rows that take a drop end.
    private var end: Int { min(count, limit ?? count) }
    private func movedAbove(_ row: Int) -> CGFloat { CGFloat(rows.filter { $0 < row }.count) * rowHeight }
    private var movingHeight: CGFloat { CGFloat(incoming ?? rows.count) * rowHeight }
    private var parting: Bool { pressed != nil || incoming != nil }

    /// rows close up over the moving ones and part at the gap.
    func shift(_ row: Int) -> CGFloat {
        guard let before, parting, !rows.contains(row) else { return 0 }
        return -movedAbove(row) + (row >= before ? movingHeight : 0)
    }

    /// The same for what sits after the droppable rows (the Autoplay heading, the note).
    var followShift: CGFloat {
        guard parting, before != nil else { return 0 }
        return movingHeight - movedAbove(end)
    }

    /// The same for the heading between the sections (Next from).
    var splitShift: CGFloat {
        guard parting, before != nil, let split else { return 0 }
        return -movedAbove(split) + (intoFirst ? movingHeight : 0)
    }

    var slot: CGRect? {
        guard parting, let before else { return nil }
        let y: CGFloat
        if let split, intoFirst, before == split {
            // The gap closes Next in Queue, above the Next from heading.
            if let last = (0..<split).last(where: { !rows.contains($0) }) {
                y = rowTop(last) - movedAbove(last) + rowHeight
            } else {
                y = rowTop(split) - splitGap - movedAbove(split)
            }
        } else if before < end {
            y = rowTop(before) - movedAbove(before)
        } else if let last = (0..<end).last(where: { !rows.contains($0) }) {
            y = rowTop(last) - movedAbove(last) + rowHeight
        } else {
            y = rows.first.map { $0 < end ? rowTop($0) : 0 } ?? 0
        }
        return CGRect(x: 0, y: y, width: 0, height: movingHeight)
    }

    func lift(row: Int, rows: [Int], at point: NSPoint) {
        guard pressed == nil, incoming == nil, let tracker, count > 1 || limit != nil || onLeave != nil else { return }
        let y = tracker.convert(point, from: nil).y
        grab = y - rowTop(row)
        windowPoint = point
        pointerY = y
        withAnimation(Self.slide) {
            pressed = row
            self.rows = rows.sorted()
            findGap()
        }
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDragged, .leftMouseUp, .keyDown]) { [weak self] event in
            guard let self, self.pressed != nil else { return event }
            switch event.type {
            case .leftMouseDragged:
                if self.leaves(event) { return nil }
                self.follow(event.locationInWindow)
            case .leftMouseUp: self.finish(drop: true)
            case .keyDown where event.keyCode == 53: self.finish(drop: false)
            case .keyDown: return nil
            default: return event
            }
            // The drag owns these, so the click after it doesn't select anything.
            return nil
        }
        timer = Timer.scheduledTimer(withTimeInterval: 1.0 / 60, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.autoscroll() }
        }
        NSCursor.closedHand.push()
    }

    /// Past the list's sides the lifted rows leave it and carry on as a drag onto a playlist (`onLeave`). True when
    /// that happened.
    func leaves(_ event: NSEvent) -> Bool {
        guard let onLeave, pressed != nil, let scroll = tracker?.enclosingScrollView else { return false }
        let box = scroll.convert(scroll.bounds, to: nil)
        let x = event.locationInWindow.x
        guard x < box.minX || x > box.maxX else { return false }
        let moving = rows
        finish(drop: false)
        onLeave(event, moving)
        return true
    }

    /// Songs from another list came over the list: the rows part for `count` songs at the pointer.
    func takeIn(count: Int, at point: NSPoint) {
        guard pressed == nil, let tracker else { return }
        grab = rowHeight / 2
        windowPoint = point
        pointerY = tracker.convert(point, from: nil).y
        // The songs and their gap arrive in one change, so views that draw from either see the gap together.
        withAnimation(Self.slide) {
            incoming = max(1, count)
            findGap()
        }
    }

    /// The pointer moved with songs from another list. Near the edges the list scrolls.
    func moveIn(_ point: NSPoint) {
        guard incoming != nil, let tracker else { return }
        windowPoint = point
        pointerY = tracker.convert(point, from: nil).y
        findGap()
        if let scroll = tracker.enclosingScrollView {
            let box = scroll.convert(scroll.bounds, to: nil)
            if EdgeScroll.scroll(scroll, by: EdgeScroll.speed(top: box.maxY - point.y, bottom: point.y - box.minY)) {
                pointerY = tracker.convert(point, from: nil).y
                findGap()
            }
        }
    }

    /// Songs from another list left (`drop` false) or were dropped. A drop returns the row they land before and whether
    /// they join the first section.
    @discardableResult func endIn(drop: Bool) -> (before: Int, intoFirst: Bool)? {
        guard incoming != nil else { return nil }
        let place = before.map { ($0, intoFirst) }
        if drop {
            // The new rows land in place in the same update, so nothing slides back.
            var still = Transaction()
            still.disablesAnimations = true
            withTransaction(still) { incoming = nil; before = nil }
        } else {
            withAnimation(Self.slide) { incoming = nil; before = nil }
        }
        return drop ? place : nil
    }

    func follow(_ point: NSPoint) {
        guard pressed != nil, let tracker else { return }
        windowPoint = point
        pointerY = tracker.convert(point, from: nil).y
        findGap()
    }

    func finish(drop: Bool) {
        guard pressed != nil else { return }
        let (moving, gap) = (rows, before)
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
        timer?.invalidate()
        timer = nil
        NSCursor.pop()
        // The new order lands in place in the same update, so nothing slides back.
        var still = Transaction()
        still.disablesAnimations = true
        withTransaction(still) {
            pressed = nil
            rows = []
            before = nil
            guard drop, let gap else { return }
            let insert = gap - moving.filter { $0 < gap }.count
            let fromBelowLimit = limit.map { l in moving.contains { $0 >= l } } ?? false
            // A drop in place still counts when it changes a song's section.
            let crosses = split.map { s in moving.contains { ($0 < s) != intoFirst } } ?? false
            if !fromBelowLimit, !crosses, moving.enumerated().allSatisfy({ $0.element == insert + $0.offset }) { return }
            onMove(moving, gap, intoFirst)
        }
    }

    /// The gap is before the first unmoved row whose middle, with the moving rows taken out, is below the card's middle.
    /// Uses the rows' own places, so rows still sliding never move it.
    private func findGap() {
        let center = pointerY - grab + rowHeight / 2
        var gap = (0..<end).first { !rows.contains($0) && center < rowTop($0) - movedAbove($0) + rowHeight / 2 } ?? end
        var first = defaultIntoFirst
        if let split {
            // The heading's middle, with the moving rows taken out, decides the section. The gap stays on its side.
            first = center < rowTop(split) - splitGap / 2 - movedAbove(split)
            gap = first ? min(gap, split) : max(gap, split)
        } else if incoming != nil, headAbove > 0 {
            // Songs from elsewhere dropped above Next from's heading start Next in Queue.
            first = center < -headAbove / 2
            if first { gap = 0 }
        }
        guard gap != before || first != intoFirst else { return }
        if before == nil { before = gap; intoFirst = first } else { withAnimation(Self.slide) { before = gap; intoFirst = first } }
    }

    private func autoscroll() {
        guard pressed != nil, let tracker, let scroll = tracker.enclosingScrollView, let point = windowPoint else { return }
        let visible = tracker.visibleRect
        let y = tracker.convert(point, from: nil).y
        if EdgeScroll.scroll(scroll, by: EdgeScroll.speed(top: y - visible.minY, bottom: visible.maxY - y)) { follow(point) }
    }
}

/// Gives a `RowDrag` the list's own coordinates: an empty view behind the rows, top down.
struct RowDragTracker: NSViewRepresentable {
    let drag: RowDrag

    func makeNSView(context: Context) -> TrackerView { TrackerView() }
    func updateNSView(_ view: TrackerView, context: Context) { drag.tracker = view }

    final class TrackerView: NSView {
        override var isFlipped: Bool { true }
        override func hitTest(_ point: NSPoint) -> NSView? { nil }
    }
}

struct RowDragSlot: View {
    let drag: RowDrag

    var body: some View {
        #if DEBUG
        let _ = { VisualStats.dragSlot["bodies", default: 0] += 1; VisualStats.dragSlot["y"] = Double(drag.slot?.minY ?? -1) }()
        #endif
        if let slot = drag.slot {
            RoundedRectangle(cornerRadius: T.rMD, style: .continuous)
                .fill(T.dragSlot.color)
                .overlay(RoundedRectangle(cornerRadius: T.rMD, style: .continuous).strokeBorder(T.dragSlotEdge.color, lineWidth: 1))
                .frame(height: max(0, slot.height - T.px(4)))
                .frame(maxWidth: .infinity)
                .offset(y: slot.minY + T.px(2))
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                .allowsHitTesting(false)
        }
    }
}

struct RowDragOverlay: View {
    let drag: RowDrag
    let accent: RGBA
    let onAccent: RGBA

    var body: some View {
        if drag.isActive, drag.rows.count > 1, let pressed = drag.pressed {
            CountBadge(text: Ln("{count} song", "{count} songs", drag.rows.count), accent: accent, onAccent: onAccent)
                .frame(maxWidth: .infinity, alignment: .trailing)
                .padding(.trailing, T.px(44))
                .offset(y: drag.rowTop(pressed) + drag.liftedOffset + T.px(6))
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                .allowsHitTesting(false)
        }
    }
}
