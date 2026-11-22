import QuartzCore
import SwiftUI

/// lines blur and fade with distance from the current one; clicking a line seeks to it.
struct LyricsView: View {
    let shell: ShellModel
    var large = false
    /// The window's width in token pixels, which picks the large type size.
    var cssWidth: CGFloat = 0
    @State private var didJump = false
    /// interpolates between the player's reports, so the sweep moves at the display's rate.
    @State private var clock = LyricClock()
    /// for the staggered move to the next line.
    @State private var tops = LyricTops()
    /// held while the window is resized, so the lines don't re-shape on every frame of the drag.
    @State private var heldFont: CGFloat?
    /// nil when the song credits no lyricist or composer.
    @State private var writers: String?
    /// how far every line starts back from its place, and a count that starts the move.
    @State private var glide = LyricGlide()
    /// nil means the current row. A gap row folds or opens in the move's own update, because in the update that changed
    /// the current line every row's dimming animation would also carry the rows' new places.
    @State private var openGap: Int?

    @Environment(\.isSnapshot) private var isSnapshot
    private var player: any PlayerViewModel { shell.player }

    fileprivate func fontSize() -> CGFloat {
        heldFont ?? (large ? T.px(min(T.lyricExpandedMax, max(T.lyricExpandedMin, cssWidth * T.lyricExpandedShare))) : T.textSection)
    }

    /// room outside the large text column for the blur and glow; the scroll view and mask widen and the text is inset to
    /// match. the dock's side padding already gives this room.
    private var blurRoom: CGFloat { large ? T.px(T.lyricBlurRoom) : 0 }

    private var lineGap: CGFloat { large ? T.px(44) : T.px(20) }

    /// as tall as its tallest bar while its gap is on; it folds to nothing otherwise.
    private var gapRowHeight: CGFloat { fontSize() * T.lyricGapHeight }

    /// `--lyric-peek` of the type size.
    private var peek: CGFloat { shell.lyricEffects ? fontSize() * T.lyricPeek : 0 }

    /// the current line scrolls to this far below the top; the line before it ends `lineGap` higher, so `peek` of it shows.
    private var lead: CGFloat { lineGap + peek }

    /// clear for `lyricTopClear` of the peek, full where the line before the current one ends.
    fileprivate func edgeFade(_ height: CGFloat) -> LinearGradient {
        let top = min(0.5, peek / max(1, height))
        return LinearGradient(stops: [.init(color: .clear, location: 0), .init(color: .clear, location: top * T.lyricTopClear),
                                      .init(color: .black, location: top), .init(color: .black, location: 0.88), .init(color: .clear, location: 1)],
                              startPoint: .top, endPoint: .bottom)
    }

    /// A unit point on a zero-height target lines up with the same point of the scroll view, so this is an exact distance.
    fileprivate func leadAnchor(_ height: CGFloat) -> UnitPoint { UnitPoint(x: 0, y: min(0.5, lead / max(1, height))) }

    var body: some View {
        lyricsBody
            .task(id: player.current?.id) {
            writers = player.current.flatMap { shell.writers(of: $0.id) }
            #if DEBUG
            VisualStats.writers = "\(player.current?.id ?? "-"): \(writers ?? "nil")"
            #endif
        }
    }

    @ViewBuilder private var lyricsBody: some View {
        #if DEBUG
        let _ = VisualStats.lyricsBodies += 1
        #endif
        #if DEBUG
        let lines = WordTimingCheck.lines(player.lyrics)
        #else
        let lines = player.lyrics
        #endif
        if lines.isEmpty && player.lyricsPending {
            // still being asked for: the space stays empty rather than saying there are none.
            Color.clear
        } else if lines.isEmpty {
            EmptyStateView(symbol: "quote.bubble", title: L("No lyrics for this song"), compact: true)
                .frame(maxHeight: .infinity, alignment: .top)
                .foregroundStyle(T.tOnArt.color)
        } else if !player.lyricsAreSynced(lines) {
            plainLyrics(lines)
        } else {
            let lines = lines.withGaps()
            GeometryReader { geo in
                ScrollViewReader { proxy in
                    let ms = Int(player.position * 1000)
                    let active = lines.shownIndex(atMs: ms)
                    let activeStart = lines[active].start ?? 0
                    let end = active + 1 < lines.count ? (lines[active + 1].start ?? activeStart + 4000) : activeStart + 4000
                    // the line fills at a singing pace; what's left before the next line is the pause.
                    let span = max(1, lines[active].fillMs(until: end))
                    // real word timing where the server has it (`songLyrics` v2), else estimated from the line's span. A bracketed line
                    // follows the span, since its word timing counts the brackets' text too.
                    let bracketed = lines[active].parts(split: shell.backingVocalsLine).backing != nil
                    // A long stretch with no lyrics: the marker fills over all of it, not the capped span.
                    let gapMs = active + 1 < lines.count ? (lines[active + 1].start ?? activeStart) - activeStart : 0
                    let inGap = lines[active].text.trimmingCharacters(in: .whitespaces).isEmpty && Double(gapMs) >= T.lyricGapMin * 1000
                    // the marker pops before the list moves on, which is `lyricMoveLead` before the next line starts.
                    let markerMs = max(1, gapMs - Int(T.lyricMoveLead * 1000))
                    let lineSpan = inGap ? markerMs : span
                    // hidden, the sweep stops with the clock and resumes from the current position when the window shows.
                    let playing = player.isPlaying && !player.isWaitingForServer && shell.windowShown
                    // A gap marker still fills with the sweep off.
                    let whole = !shell.lyricSweep && !inGap
                    let timed = lines[active].words.isEmpty || bracketed || whole ? nil : lines[active]
                    // the last word glows through the pause after the line, or, with word timing, once it's held long enough.
                    let holdFrom = timed?.words.last.map { Int($0.startMs) + Int(T.lyricHoldMin * 1000) } ?? activeStart + span
                    // the list moves on before the next line starts, and the glow has faded by then.
                    let leave = min(end, lines.leaveMs(active) ?? end)
                    let hold: Range<Int>? = !inGap && leave - holdFrom >= Int(T.lyricHoldMin * 1000) || (timed != nil && holdFrom < leave) ? holdFrom..<leave : nil
                    // A glowing last word's letters roll from its start until the list moves on, only on the server's word timing.
                    let waveFrom = hold == nil ? nil : timed?.words.last.map { Int($0.startMs) }
                    let wave = waveFrom.flatMap { from in leave > from ? from..<leave : nil }
                    // a held word's own timing can run past the moment the list moves on: its fill is done before the glow starts
                    // to fade, or the glow fading would leave its last letters unlit while the line is current.
                    let sung = timed.map { line in hold.map { line.filled(by: $0.upperBound - Int(T.lyricGlowFade * 1000)) } ?? line }
                    let sweep = (bracketed ? nil : (sung ?? lines[active]).sungFraction(atMs: ms)) ?? max(0, min(1, Double(ms - activeStart) / Double(span)))
                    let lineSweep = inGap ? max(0, min(1, Double(ms - activeStart) / Double(markerMs))) : sweep
                    // Paused, the sweep is a plain value, so a seek while paused still redraws the line.
                    let timing = playing ? LyricTiming(clock: clock, start: activeStart, span: lineSpan, playing: true, line: sung, hold: hold, wave: wave, whole: whole)
                                         : LyricTiming(start: activeStart, span: lineSpan, fixedProgress: lineSweep, line: sung, fixedMs: ms, hold: hold, wave: wave, whole: whole)
                    let lineRange = isSnapshot ? active..<min(active + 9, lines.count) : lines.startIndex..<lines.endIndex
                    let gapRows = shell.lyricEffects ? lines.gapRows() : []
                    // another song's lines are new rows; changed in place, the old letters would dissolve into the new ones.
                    let song = lines.reduce(into: Hasher()) { $0.combine($1.text); $0.combine($1.start) }.finalize()
                    Group {
                        if isSnapshot {
                            snapshotBody(lines, lineRange, active: active, sweep: lineSweep, inGap: inGap)
                        } else {
                    ScrollView(.vertical) {
                        // each row carries its own gap below it, so a gap marker's row can fold away completely.
                        VStack(alignment: .leading, spacing: 0) {
                            let openGap = openGap ?? active
                            ForEach(lines.indices, id: \.self) { index in
                                LyricLineView(line: lines[index].parts(split: shell.backingVocalsLine), distance: abs(index - active), isActive: index == active,
                                              showsBacking: index <= active, gap: index == active && inGap,
                                              gapRow: gapRows.contains(index) ? gapRowHeight : nil,
                                              // sharp during a live resize, so each frame isn't re-blurred.
                                              timing: index == active ? timing : nil, fontSize: fontSize(), blurByDistance: shell.lyricBlur && !shell.liveResizing,
                                              effects: shell.lyricEffects, sweep: shell.lyricSweep, still: !shell.liveResizing) {
                                    if let start = lines[index].start { player.seek(to: Double(start) / 1000) }
                                }
                                .equatable()
                                // folds at once with the line move (`LyricGlide`), which carries the whole visible change.
                                .frame(height: gapRows.contains(index) && index != openGap ? 0 : nil, alignment: .top)
                                .transaction { $0.animation = nil }
                                .padding(.bottom, gapRows.contains(index) && index != openGap ? 0 : lineGap)
                                .keyframeAnimator(initialValue: CGFloat(0), trigger: glide.count) { content, y in
                                    let _ = tops.offset[index] = y
                                    content.offset(y: LyricGlide.noted(y, index == active ? "lyricActive" : index == active + 3 ? "lyricBelow3" : nil, row: index, near: abs(index - active) <= 2))
                                } keyframes: { _ in
                                    glide.track(index: index, delay: index > active ? Double(min(index - active, T.lyricStaggerLines)) * T.lyricStagger : 0)
                                }
                                // outside the move's offset, so they're the line's own place. the anchor id has its own type, since a bare index
                                // would match the ForEach row and `scrollTo` would take the row.
                                .background(alignment: .top) { Color.clear.frame(height: 0).id(LyricAnchor(index: index)) }
                                .onGeometryChange(for: CGFloat.self, of: { $0.frame(in: .named(LyricTops.space)).minY }) { [tops] top in
                                    tops.at[index] = top
                                    LyricGlide.notedTop(top, row: index)
                                }
                            }
                            // moves with the lines, as the row after the last one.
                            writtenBy
                                .keyframeAnimator(initialValue: CGFloat(0), trigger: glide.count) { content, y in
                                    let _ = tops.offset[lines.count] = y
                                    content.offset(y: y)
                                } keyframes: { _ in
                                    glide.track(index: lines.count, delay: Double(min(max(lines.count - active, 0), T.lyricStaggerLines)) * T.lyricStagger)
                                }
                        }
                        .id(song)
                        .coordinateSpace(.named(LyricTops.space))
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.top, lead)
                        .padding(.horizontal, large ? blurRoom : T.px(32))
                        // room for the last line to reach the lead, however tall the panel is.
                        .padding(.bottom, max(geo.size.width * (large ? 0.70 : 0.62), geo.size.height - lead))
                    }
                    .scrollIndicators(.never)
                        }
                    }
                    .mask(edgeFade(geo.size.height))
                    .padding(.horizontal, -blurRoom) // Outside the mask, so the mask gets the wider frame too.
                    .onChange(of: player.position, initial: true) { _, position in clock.report(position, playing: playing) }
                    .onChange(of: playing) { _, now in clock.report(player.position, playing: now) }
                    .onChange(of: active) { old, line in
                        #if DEBUG
                        MotionCheck.note("switch", Double(line))
                        #endif
                        // just shown again: land on the current line rather than gliding from where it was hidden.
                        if CACurrentMediaTime() - shell.shownAt < 0.5 {
                            #if DEBUG
                            VisualStats.lyricJumps += 1
                            #endif
                            withTransaction(Transaction(animation: nil)) { openGap = line }
                            proxy.scrollTo(LyricAnchor(index: line), anchor: leadAnchor(geo.size.height))
                            return
                        }
                        #if DEBUG
                        VisualStats.lyricGlides += 1
                        #endif
                        // jump to the new line with every line held where it was, then the lines move up, each lower one a little later.
                        var jump = Transaction(animation: nil)
                        jump.disablesAnimations = true
                        withTransaction(jump) {
                            openGap = line
                            if !Motion.isReduced, let from = tops.at[old], let to = tops.at[line], from != to {
                                // A folding or opening gap row moves the rows after it at once; the move takes that out, so nothing jumps. A
                                // closing row uses its laid-out height (it can differ by rounding); an opening one isn't laid out yet, so tokens.
                                let fold = gapRowHeight + lineGap
                                var changes: [(after: Int, by: CGFloat)] = []
                                if gapRows.contains(old) { changes.append((old, -((tops.at[old + 1]).map { $0 - (tops.at[old] ?? $0) } ?? fold))) }
                                if gapRows.contains(line) { changes.append((line, fold)) }
                                glide = LyricGlide(count: glide.count + 1, shift: to - from, current: line, changes: changes, plain: !shell.lyricEffects, carry: tops.offset)
                            }
                            proxy.scrollTo(LyricAnchor(index: line), anchor: leadAnchor(geo.size.height))
                        }
                    }
                    .onChange(of: song) { DispatchQueue.main.async { settle(proxy, active, geo.size.height) } }
                    // A new size rewraps the lines: the current one goes straight back to its place.
                    .onChange(of: geo.size) {
                        // during a live resize, follow only once the current line has drifted visibly.
                        guard shell.liveResizing else { settle(proxy, active, geo.size.height); return }
                        if let top = tops.at[active], abs(top - (tops.settled ?? top)) > T.lyricResizeDrift { settle(proxy, active, geo.size.height) }
                    }
                    // A resize holds the type size and lands once, when it ends.
                    .onChange(of: shell.liveResizing) { _, resizing in
                        heldFont = nil
                        if resizing { heldFont = fontSize() } else { DispatchQueue.main.async { settle(proxy, active, geo.size.height) } }
                    }
                    .onAppear {
                        guard !didJump else { return }
                        didJump = true
                        proxy.scrollTo(LyricAnchor(index: active), anchor: leadAnchor(geo.size.height)) // Jumps into place when the view opens, then glides between lines.
                    }
                }
            }
        }
    }
}

extension LyricsView {
    /// smaller and dimmer after the last line.
    @ViewBuilder var writtenBy: some View {
        if let writers {
            Text("\(Text(L("Written By:")).fontWeight(.bold)) \(writers)")
                .font(.navia(fontSize() * T.lyricsCreditScale))
                .foregroundStyle(T.tOnArt.color)
                .opacity(T.lyricsCreditOpacity)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

extension LyricsView {
    /// after the lines re-wrapped for a new window size.
    func settle(_ proxy: ScrollViewProxy, _ active: Int, _ height: CGFloat) {
        tops.settled = tops.at[active]
        var still = Transaction(animation: nil)
        still.disablesAnimations = true
        withTransaction(still) { proxy.scrollTo(LyricAnchor(index: active), anchor: leadAnchor(height)) }
    }
}

private struct LyricAnchor: Hashable { let index: Int }

/// every line starts `shift` below its place and springs into it (`--lyric-bounce`).
private struct LyricGlide {
    var count = 0
    var shift: CGFloat = 0
    var current = 0
    /// every row after `after` moved by `by` in the layout.
    var changes: [(after: Int, by: CGFloat)] = []
    /// lyric effects off: every line moves together in a plain ease.
    var plain = false
    /// A new move starts from where the previous one got to, so a running move carries on.
    var carry: [Int: CGFloat] = [:]

    /// the scroll's move, less what the layout already moved the row.
    func start(_ index: Int) -> CGFloat {
        func moved(_ row: Int) -> CGFloat { changes.reduce(0) { $0 + (row > $1.after ? $1.by : 0) } }
        return shift + moved(current) - moved(index) + (carry[index] ?? 0)
    }

    func track(index: Int, delay: Double) -> KeyframeTrack<CGFloat, CGFloat, some KeyframeTrackContent<CGFloat>> {
        let shift = start(index)
        return KeyframeTrack {
            MoveKeyframe(shift)
            // never zero long: the spring takes its starting speed from this hold, and 0/0 sends the line off screen.
            LinearKeyframe(shift, duration: plain ? T.lyricMoveDelayMin : max(delay, T.lyricMoveDelayMin))
            // given time to really come to rest so it lands exactly on the line; cut at `lyricMove` it stopped short.
            SpringKeyframe(0, duration: T.lyricMoveSettle, spring: Spring(duration: T.lyricMove, bounce: plain ? 0 : T.lyricBounce))
            LinearKeyframe(0, duration: T.lyricMoveDelayMin)
        }
    }
    /// check runs: each frame's offset of the current line and one lower down (`motionSample`).
    static func noted(_ y: CGFloat, _ key: String?, row: Int, near: Bool) -> CGFloat {
        #if DEBUG
        if let key { MotionCheck.note(key, y) }
        if near { MotionCheck.note("offset\(row)", y) }
        #endif
        return y
    }
    /// check runs: each line's place in the column as the layout changes.
    static func notedTop(_ top: CGFloat, row: Int) {
        #if DEBUG
        MotionCheck.note("top\(row)", top)
        #endif
    }
}

/// not observed: read only when the current line changes.
@MainActor private final class LyricTops {
    static let space = "lyricsColumn"
    var at: [Int: CGFloat] = [:]
    var offset: [Int: CGFloat] = [:]
    var settled: CGFloat?
}

private extension LyricsView {
    /// Offscreen check images can't draw a scroll view, so they show the current line and those after it.
    func snapshotBody(_ lines: [LyricLine], _ range: Range<Int>, active: Int, sweep: Double, inGap: Bool) -> some View {
        VStack(alignment: .leading, spacing: lineGap) {
            ForEach(range, id: \.self) { index in
                LyricLineView(line: lines[index].parts(split: shell.backingVocalsLine), distance: abs(index - active), isActive: index == active,
                              showsBacking: index <= active, gap: index == active && inGap,
                              timing: index == active ? .fixed(shell.lyricSweep || inGap ? sweep : 1) : nil, fontSize: fontSize(), blurByDistance: shell.lyricBlur,
                              effects: shell.lyricEffects, sweep: shell.lyricSweep, action: {})
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.top, lead)
        .padding(.horizontal, large ? blurRoom : T.px(32))
    }

    /// Unsynced lyrics as plain text, with no blur, highlight or seeking.
    func plainLyrics(_ lines: [LyricLine]) -> some View {
        GeometryReader { geo in
            ScrollView(.vertical) {
                VStack(alignment: .leading, spacing: large ? T.px(T.lyricPlainGap) : T.px(10)) {
                    ForEach(lines.indices, id: \.self) { Text(lines[$0].text).font(.navia(large ? fontSize() * T.lyricPlainScale : fontSize(), .semibold)).foregroundStyle(T.tOnArt.color).opacity(T.lyricPlain) }
                    writtenBy
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.top, lead)
                .padding(.horizontal, large ? blurRoom : T.px(32))
                .padding(.bottom, T.px(40))
            }
            .scrollIndicators(.never)
            .mask(edgeFade(geo.size.height))
            .padding(.horizontal, -blurRoom)
        }
    }
}

/// Playback position between the player's reports, which come a few times a second. The sweep reads this every
/// display frame so it moves smoothly.
@MainActor final class LyricClock {
    private var anchor: Double = 0
    private var anchorTime: CFTimeInterval = 0
    private var playing = false
    /// each report re-times them.
    private let followers = NSHashTable<LyricLayerView>.weakObjects()

    func follow(_ view: LyricLayerView) { followers.add(view) }
    func unfollow(_ view: LyricLayerView) { followers.remove(view) }

    func report(_ position: Double, playing: Bool) {
        let now = CACurrentMediaTime()
        let predicted = estimate(at: now)
        // Reports jitter a little against the clock. Blend small differences so the sweep never steps back, but take
        // seeks and track changes at once.
        if playing, self.playing, abs(position - predicted) < 0.25 {
            anchor = predicted + (position - predicted) * 0.3
        } else {
            anchor = position
        }
        anchorTime = now
        self.playing = playing
        for view in followers.allObjects { view.sync() }
    }

    /// Never runs more than 0.75 s past the last report, so a stalled player can't run the sweep ahead.
    func estimate(at now: CFTimeInterval = CACurrentMediaTime()) -> Double {
        playing ? anchor + min(max(0, now - anchorTime), 0.75) : anchor
    }
}

/// from the clock, or a fixed value for check images.
struct LyricTiming: Equatable {
    var clock: LyricClock?
    var start = 0
    var span = 1
    var playing = false
    var fixedProgress: Double? = nil
    /// The line, when it has word timing to follow instead of the span.
    var line: LyricLine? = nil
    /// ms, while paused.
    var fixedMs: Int? = nil
    /// ms of the song.
    var hold: Range<Int>? = nil
    /// ms of the song: the held last word's letters roll from its start until the line is left.
    var wave: Range<Int>? = nil
    /// lit whole, with no rising words.
    var whole = false

    /// A part of the line filled after another, as its own timing; only the part sung `last` keeps the held-word glow.
    func part(_ from: Double, _ to: Double, last: Bool) -> LyricTiming {
        var part = self
        part.start = start + Int(from * Double(span))
        part.span = max(1, Int((to - from) * Double(span)))
        part.fixedProgress = fixedProgress.map { max(0, min(1, ($0 - from) / max(0.0001, to - from))) }
        if !last { part.hold = nil; part.wave = nil }
        return part
    }

    /// ms.
    func nowMs() -> Double { Double(fixedMs ?? Int((clock?.estimate() ?? 0) * 1000)) }

    /// 0 to 1, fading over `lyricGlowFade`; `at` is in ms.
    func holdStrength(at time: Double? = nil) -> Double {
        guard let hold else { return 0 }
        let ms = time ?? nowMs()
        let fade = T.lyricGlowFade * 1000
        return max(0, min(1, (ms - Double(hold.lowerBound)) / fade, (Double(hold.upperBound) - ms) / fade))
    }

    static func fixed(_ progress: Double) -> LyricTiming { LyricTiming(fixedProgress: progress) }

    /// each letter of the text's last word raised as a share of the type size while it rolls; empty when it doesn't.
    func waves(in text: String, at time: Double? = nil) -> [Double] {
        guard let wave, !Motion.isReduced, !whole else { return [] }
        let since = ((time ?? nowMs()) - Double(wave.lowerBound)) / 1000
        return (0..<LyricWave.letters(in: text)).map { LyricWave.rise(letter: $0, at: since, held: Double(wave.count) / 1000) }
    }

    /// without word timing a word is a run between spaces.
    func words(in text: String) -> [LyricWordSpan] {
        if let line {
            let utf8 = line.text.utf8
            func chars(_ offset: UInt32) -> Int? {
                String.Index(utf8.index(utf8.startIndex, offsetBy: min(Int(offset), utf8.count)), within: line.text)
                    .map { line.text.distance(from: line.text.startIndex, to: $0) }
            }
            return line.words.compactMap { word in
                guard let from = chars(word.start), let to = chars(word.end), from < to else { return nil }
                return LyricWordSpan(chars: from..<to, startMs: Double(word.startMs))
            }
        }
        var words: [LyricWordSpan] = [], from: Int?
        for (index, character) in text.enumerated() {
            if character.isWhitespace {
                if let start = from { words.append(LyricWordSpan(chars: start..<index, startMs: nil)) }
                from = nil
            } else if from == nil {
                from = index
            }
        }
        if let start = from { words.append(LyricWordSpan(chars: start..<text.count, startMs: nil)) }
        return words
    }

    /// 0 to 1, nil before it starts; `at` is in ms. A word rises whole over `lyricWordLiftTime` however long it's sung,
    /// starting as the glow ahead of the fill reaches it, and stays up. `count` is the text's length in characters.
    func rise(of word: LyricWordSpan, count: Int, at time: Double? = nil) -> Double? {
        let since: Double
        if let start = word.startMs {
            since = (time ?? nowMs()) - start
        } else {
            // before the line starts the time to go counts too, so nothing rises early.
            let early = min(0, (time ?? nowMs()) - Double(start))
            since = early + (progress(at: time) - Double(word.chars.lowerBound) / Double(max(1, count))) * Double(span)
        }
        let ms = since + T.lyricWordLiftLead * 1000
        return ms > 0 ? T.lyricWordLiftCurve.value(at: min(1, ms / (T.lyricWordLiftTime * 1000))) : nil
    }

    func wordLifts(in text: String) -> [LyricWordLift] {
        guard !Motion.isReduced, !whole else { return [] }
        let count = text.count
        return words(in: text).compactMap { word in rise(of: word, count: count).map { LyricWordLift(chars: word.chars, rise: $0) } }
    }

    /// 0 to 1; `at` is in ms of the song.
    func progress(at time: Double? = nil) -> Double {
        if whole { return 1 }
        if time == nil, let fixedProgress { return fixedProgress }
        guard let ms = time ?? clock.map({ $0.estimate() * 1000 }) else { return 0 }
        if let line, let sung = line.sungFraction(atMs: Int(ms)) { return sung }
        return max(0, min(1, (ms - Double(start)) / Double(span)))
    }

    nonisolated static func == (a: Self, b: Self) -> Bool {
        a.clock === b.clock && a.start == b.start && a.span == b.span && a.playing == b.playing && a.fixedProgress == b.fixedProgress && a.line == b.line
            && a.fixedMs == b.fixedMs && a.hold == b.hold && a.wave == b.wave && a.whole == b.whole
    }
}

/// Draws a lyric line, and on the current line the sweep. The sweep fills wrapped text one visual line at a time,
/// in reading order.
struct LyricSweepRenderer: TextRenderer, Animatable {
    var progress: Double
    var active: Bool
    var timed = false
    var lifts: [LyricWordLift] = []
    /// whole device pixels; 0 under Reduce Motion.
    var lift: CGFloat = 0
    /// in points.
    var pixel: CGFloat = 1
    /// 0 to 1.
    var hold: Double = 0
    /// counted from the end of the text.
    var lastWord = 0
    /// trailing spaces after the last word, in glyphs.
    var trailing = 0
    /// points each letter of the last word rolls up (`LyricTiming.waves`), in text order.
    var wave: [CGFloat] = []
    /// the whole line, or only the held word's glow (the layer-drawn fill's glow image).
    var part = Part.all
    /// which glyphs are drawn, by index in the text, for the layer-drawn fill's separate letter images; nil draws all.
    var drawn: ((Int) -> Bool)? = nil

    enum Part { case all, glow }

    var animatableData: Double {
        get { progress }
        set { progress = newValue }
    }

    /// the context puts glyphs on whole device pixels, so a word between two pixels is drawn on both, weighted, in a
    /// layer where the two add up: it moves smoothly and rests sharp. `first` is its first glyph's index in the text.
    /// `total` is the text's glyph count, where the rolling letters end.
    private func draw(_ line: Text.Layout.Line, first: Int, total: Int, in context: GraphicsContext) {
        guard lift > 0 && !lifts.isEmpty || !wave.isEmpty || drawn != nil else { context.draw(line); return }
        var glyph = first
        var raised: [(slice: Text.Layout.RunSlice, pixels: CGFloat)] = []
        let rolling = total - wave.count
        for run in line {
            for slice in run {
                defer { glyph += 1 }
                if let drawn, !drawn(glyph) { continue }
                let roll = glyph >= rolling && glyph < total ? wave[glyph - rolling] : 0
                raised.append((slice, (lift * (lifts.first { $0.chars.contains(glyph) }?.rise ?? 0) + roll) / pixel))
            }
        }
        func pass(_ target: GraphicsContext, upper: Bool) {
            for (slice, pixels) in raised {
                let whole = pixels.rounded(.down), part = pixels - whole
                if upper && part == 0 { continue }
                var glyph = target
                glyph.opacity = target.opacity * (upper ? part : 1 - part)
                let up = whole + (upper ? 1 : 0)
                if up != 0 { glyph.translateBy(x: 0, y: -(up + Self.pixelNudge) * pixel) }
                glyph.draw(slice)
            }
        }
        guard raised.contains(where: { $0.pixels != $0.pixels.rounded(.down) }) else { pass(context, upper: false); return }
        context.drawLayer { layer in
            pass(layer, upper: false)
            var added = layer
            added.blendMode = .plusLighter
            pass(added, upper: true)
        }
    }

    /// shared by the renderer and the layer-drawn fill, so both lay it out the same.
    static func text(_ text: String, size: CGFloat) -> Text {
        Text(verbatim: text).font(.navia(size, .bold)).tracking(-0.01 * size).foregroundStyle(T.tOnArt.color)
    }

    /// so rounding error can't leave a glyph one pixel short.
    private static let pixelNudge: CGFloat = 0.01

    func draw(layout: Text.Layout, in context: inout GraphicsContext) {
        guard active else {
            var first = 0
            for line in layout {
                if drawn == nil { context.draw(line) } else { draw(line, first: first, total: .max, in: context) }
                first += line.reduce(0) { $0 + $1.count }
            }
            return
        }
        let boxes = LyricFillLine.lines(of: layout)
        let total = boxes.reduce(0) { $0 + $1.count }
        // where the last word ends, past any trailing spaces, as the glow counts it.
        let lettered = total - trailing
        var filled = progress * Double(max(1, total))
        for (line, box) in zip(layout, boxes) {
            let reached = max(0, min(Double(box.count), filled))
            let fraction = box.count == 0 ? 1 : reached / Double(box.count)
            filled -= Double(box.count)
            let bounds = box.bounds, band = box.band, rtl = box.rtl
            let (edge, soft) = box.edge(reached: reached, timed: timed)
            if part == .all {
                var base = context
                base.opacity = T.lyricUnsung.a
                draw(line, first: box.start, total: lettered, in: base)
            }
            guard fraction > 0 else { continue }
            // the word is drawn into its own layer and only that layer's shadow shows, so the halo follows the letters.
            if hold > 0, lastWord > 0, box.start + box.count == total {
                let word = box.glyphs.suffix(min(lastWord, box.glyphs.count)).reduce(CGRect.null) { $0.union($1) }
                let area = word.insetBy(dx: 0, dy: -bounds.height * 0.5)
                for halo in T.lyricGlow {
                    var glow = context
                    glow.opacity = hold
                    glow.addFilter(.shadow(color: halo.color.color, radius: bounds.height * halo.radius, options: .shadowOnly))
                    glow.drawLayer { layer in
                        layer.clip(to: Path(area))
                        draw(line, first: box.start, total: lettered, in: layer)
                    }
                }
            }
            guard part == .all else { continue }
            var lit = context
            lit.clipToLayer { mask in
                let full = rtl ? CGRect(x: edge, y: band.minY, width: band.maxX - edge, height: band.height)
                               : CGRect(x: band.minX, y: band.minY, width: edge - band.minX, height: band.height)
                mask.fill(Path(full), with: .color(.black))
                let fade = CGRect(x: rtl ? edge - soft : edge, y: band.minY, width: soft, height: band.height)
                mask.fill(Path(fade), with: .linearGradient(Gradient(colors: [.black, .clear]),
                                                            startPoint: CGPoint(x: rtl ? edge : edge, y: 0),
                                                            endPoint: CGPoint(x: rtl ? edge - soft : edge + soft, y: 0)))
            }
            draw(line, first: box.start, total: lettered, in: lit)
        }
    }
}

struct LyricFillLine: Equatable {
    var bounds: CGRect
    var rtl: Bool
    /// its first glyph's index in the text.
    var start: Int
    var glyphs: [CGRect]

    var count: Int { glyphs.count }
    /// generous vertical room so ascenders, descenders and accents aren't clipped.
    var band: CGRect { bounds.insetBy(dx: -bounds.height, dy: -bounds.height * 0.5) }

    static func lines(of layout: Text.Layout) -> [LyricFillLine] {
        var start = 0
        return layout.map { line in
            let glyphs = line.flatMap { run in run.indices.map { run[$0].typographicBounds.rect } }
            defer { start += glyphs.count }
            return LyricFillLine(bounds: line.typographicBounds.rect, rtl: line.first?.layoutDirection == .rightToLeft, start: start, glyphs: glyphs)
        }
    }

    /// with word timing (`timed`) the edge sits inside the glyph being sung, with a narrow soft edge.
    func edge(reached: Double, timed: Bool) -> (edge: CGFloat, soft: CGFloat) {
        let fraction = count == 0 ? 1 : reached / Double(count)
        guard timed else {
            let edge = fraction >= 1 ? (rtl ? band.minX : band.maxX)
                : rtl ? bounds.maxX - fraction * bounds.width : bounds.minX + fraction * bounds.width
            return (edge, max(1, bounds.width * 0.14))
        }
        let k = min(glyphs.count - 1, Int(reached))
        let part = reached - Double(k)
        let box = glyphs.isEmpty ? bounds : glyphs[max(0, k)]
        let soft = max(1, bounds.height * T.lyricWordSoftEdge)
        let edge = fraction >= 1 ? (rtl ? bounds.minX - soft : bounds.maxX + soft)
            : rtl ? box.maxX - part * box.width : box.minX + part * box.width
        return (edge, soft)
    }
}

/// the bracketed part is the backing vocals, without the brackets.
nonisolated struct LyricParts: Equatable, Sendable {
    var main: String
    var backing: String?
    /// so it's sung first.
    var backingFirst = false
}

extension LyricLine {
    /// unsplit (backing line switch off), the line exactly as written.
    func parts(split: Bool) -> LyricParts { split ? parts : LyricParts(main: text, backing: nil) }

    /// unbalanced brackets leave the line as it is.
    var parts: LyricParts {
        guard text.contains("(") else { return LyricParts(main: text, backing: nil) }
        var outside = "", inside: [String] = [], current = "", depth = 0
        for character in text {
            switch character {
            case "(":
                depth += 1
                if depth == 1 { current = ""; continue }
            case ")":
                guard depth > 0 else { return LyricParts(main: text, backing: nil) }
                depth -= 1
                if depth == 0 { inside.append(current); continue }
            default: break
            }
            if depth > 0 { current.append(character) } else { outside.append(character) }
        }
        guard depth == 0 else { return LyricParts(main: text, backing: nil) }
        func tidy(_ s: String) -> String { s.split(whereSeparator: \.isWhitespace).joined(separator: " ") }
        let backing = tidy(inside.map(tidy).joined(separator: " "))
        let first = text.trimmingCharacters(in: .whitespaces).hasPrefix("(")
        return LyricParts(main: tidy(outside), backing: backing.isEmpty ? nil : backing, backingFirst: first)
    }
}

extension LyricLine {
    /// the same line with its word timing cut to end by `ms` (song ms), so its fill is complete then.
    func filled(by ms: Int) -> LyricLine {
        let cap = UInt32(max(0, ms))
        var line = self
        line.words = words.indices.map { i in
            let word = words[i]
            let end = word.endMs ?? (i + 1 < words.count ? words[i + 1].startMs : endMs.map { UInt32(max(0, $0)) } ?? word.startMs)
            return LyricWord(start: word.start, end: word.end, startMs: min(word.startMs, cap), endMs: min(end, cap))
        }
        line.endMs = endMs.map { min($0, ms) }
        return line
    }

    /// in ms: characters at `lyricFillPace`, capped by the time to the next line, `lyricLineMax` and the server's end.
    func fillMs(until next: Int) -> Int {
        let start = start ?? 0
        let shown = parts.main.isEmpty ? parts.backing ?? "" : parts.main
        let paced = Int(Double(shown.count) / T.lyricFillPace * 1000)
        return Swift.min(paced, next - start, Int(T.lyricLineMax * 1000), (endMs ?? Int.max) - start)
    }
}

extension [LyricLine] {
    /// the next line takes over `lyricMoveLead` before it starts, so the move lands as its fill begins, but never before
    /// the line before it has finished filling.
    func shownIndex(atMs ms: Int) -> Int {
        let now = activeIndex(atMs: ms)
        guard let leave = leaveMs(now) else { return now }
        return ms >= leave ? now + 1 : now
    }

    /// when the list moves on from line `index` to the next; nil for the last line.
    func leaveMs(_ index: Int) -> Int? {
        guard index + 1 < count, let next = self[index + 1].start else { return nil }
        let line = self[index]
        let lead = next - Int(T.lyricMoveLead * 1000)
        let filled = line.text.trimmingCharacters(in: .whitespaces).isEmpty ? lead : (line.start ?? 0) + line.fillMs(until: next)
        return Swift.max(lead, filled)
    }

    /// their rows only take room while their gap is on.
    func gapRows() -> Set<Int> {
        Set(indices.filter { index in
            guard self[index].text.trimmingCharacters(in: .whitespaces).isEmpty, index + 1 < count,
                  let start = self[index].start, let next = self[index + 1].start else { return false }
            return Double(next - start) >= T.lyricGapMin * 1000
        })
    }

    /// adds empty lines where a long stretch has no lyrics, so the gap marker has a row; existing empty lines stay.
    func withGaps() -> [LyricLine] {
        let gapMs = Int(T.lyricGapMin * 1000)
        var out: [LyricLine] = []
        if let first = first?.start, first >= gapMs { out.append(LyricLine(start: 0, text: "")) }
        for (index, line) in enumerated() {
            out.append(line)
            guard index + 1 < count, let start = line.start, let next = self[index + 1].start,
                  !line.text.trimmingCharacters(in: .whitespaces).isEmpty else { continue }
            let end = start + line.fillMs(until: next)
            if next - end >= gapMs { out.append(LyricLine(start: end, text: "")) }
        }
        return out
    }
}

/// the held last word's rolling letters.
enum LyricWave {
    /// scripts whose letters join, stack or reorder when shaped (Arabic, Syriac, N'Ko, Indic, Southeast Asian, Tibetan,
    /// Mongolian and the joiners), so their letters can't be moved one by one.
    nonisolated static let joinedScripts: [ClosedRange<UInt32>] = [
        0x0600...0x08FF, 0x0900...0x0DFF, 0x0E00...0x0FFF, 0x1000...0x109F, 0x1780...0x18AF, 0x1A00...0x1AAF, 0x1B00...0x1BFF,
        0x200C...0x200D, 0xA840...0xA87F, 0xA980...0xA9DF, 0xFB50...0xFDFF, 0xFE70...0xFEFF, 0x10D00...0x11FFF,
    ]

    /// the last word's letters (characters, so grapheme clusters) when each can move by itself: one Unicode scalar, not
    /// from a joined script; else 0 and the word stays whole.
    nonisolated static func letters(in text: String) -> Int {
        let word = text.reversed().drop { $0.isWhitespace }.prefix { !$0.isWhitespace }
        let loose = word.allSatisfy { letter in
            letter.unicodeScalars.count == 1 && !joinedScripts.contains { $0.contains(letter.unicodeScalars.first!.value) }
        }
        return loose ? word.count : 0
    }

    /// letter `letter` raised as a share of the type size, `since` seconds after the word started in a hold of `held`
    /// seconds; at rest at both ends.
    nonisolated static func rise(letter: Int, at since: Double, held: Double) -> Double {
        guard since > 0, since < held else { return 0 }
        func smooth(_ x: Double) -> Double { let x = max(0, min(1, x)); return x * x * (3 - 2 * x) }
        let envelope = smooth(since / T.lyricWaveIn) * smooth((held - since) / T.lyricWaveOut)
        return T.lyricWaveHeight * envelope * (0.5 - 0.5 * cos(2 * .pi * (since / T.lyricWavePeriod - Double(letter) * T.lyricWavePhase)))
    }
}

struct LyricWordSpan: Equatable {
    let chars: Range<Int>
    /// ms of the song.
    let startMs: Double?
}

/// rise 0 to 1.
struct LyricWordLift: Equatable {
    let chars: Range<Int>
    let rise: Double
}

private struct LyricLineView: View, Equatable {
    let line: LyricParts
    let distance: Int
    let isActive: Bool
    /// shows once its line has been reached.
    var showsBacking = false
    var gap = false
    /// its height while its gap is on; nil for a lyric line.
    var gapRow: CGFloat? = nil
    /// nil on the other lines.
    let timing: LyricTiming?
    let fontSize: CGFloat
    /// The Settings option. Dimming applies either way.
    var blurByDistance = true
    /// off: no glow, lift or gap bars; the sweep has its own switch.
    var effects = true
    /// off, the line lights whole at once: no fill, no rising words and so no fall.
    var sweep = true
    /// false during a live resize: the lines re-wrap and layers stay out of it.
    var still = true
    let action: () -> Void
    @State private var hovering = false
    @Environment(\.displayScale) private var displayScale
    /// A share of the lift: 1 while current, then down past 0 to a dip of `lyricLiftBounce` and back to rest.
    @State private var risen: CGFloat = 0
    @State private var fall = 0
    /// so it can open from nothing without a jump.
    @State private var backingHeight: CGFloat = 0
    /// once settled, Core Animation layers take over its fill (`LyricFill`, `LyricGapBars`), so nothing redraws per frame.
    @State private var layered = false
    @Environment(\.isSnapshot) private var isSnapshot
    /// off in check runs that compare the two ways of drawing.
    private static let layers = !UserDefaults.standard.bool(forKey: "naviaLyricLayersOff")

    nonisolated static func == (a: Self, b: Self) -> Bool {
        a.line == b.line && a.distance == b.distance && a.isActive == b.isActive && a.showsBacking == b.showsBacking && a.gap == b.gap && a.gapRow == b.gapRow
            && a.timing == b.timing && a.fontSize == b.fontSize && a.blurByDistance == b.blurByDistance && a.effects == b.effects && a.sweep == b.sweep && a.still == b.still
    }

    private var blur: CGFloat { distance == 0 || hovering || !blurByDistance ? 0 : T.px(min(T.lyricBlurNear + CGFloat(distance) * T.lyricBlurStep, T.lyricBlurMax)) }
    /// up by the lift, then under its line by the dip, then at rest; on whole device pixels so risen words rest sharp.
    private var fallOffset: CGFloat {
        let lift = lift(mainSize)
        guard lift > 0 else { return 0 }
        let dip = max(1 / displayScale, (lift * T.lyricLiftBounce * displayScale).rounded() / displayScale)
        return risen >= 0 ? -lift * risen : dip * -risen / T.lyricLiftBounce
    }

    private func settle(_ active: Bool) {
        fall += 1
        let run = fall
        if active { risen = 1; return }
        guard risen > 0 else { return }
        withAnimation(T.easeOut(T.lyricWordFall)) { risen = -T.lyricLiftBounce } completion: {
            guard run == fall else { return }
            withAnimation(T.easeInOut(T.lyricLiftSettle)) { risen = 0 }
        }
    }

    /// with the sweep off a lyric line has nothing to animate, so only a gap row takes the layers.
    private var takesLayers: Bool { (sweep || effects && gapRow != nil) && still && !isSnapshot && Self.layers }

    private func lift(_ size: CGFloat) -> CGFloat { Motion.isReduced || !effects || !sweep ? 0 : (size * T.lyricWordLift * displayScale).rounded() / displayScale }
    private var opacity: Double { isActive ? 1 : hovering ? T.lyricHover : max(T.lyricDimMin, T.lyricDimNear - Double(distance) * T.lyricDimStep) }
    /// then it's the bracketed part alone, at the smaller size.
    private var alone: Bool { line.main.isEmpty && line.backing != nil }
    private var mainText: String { alone ? line.backing ?? "" : line.main }
    private var mainSize: CGFloat { alone ? fontSize * T.lyricBackingScale : fontSize }

    var body: some View {
        Button(action: action) {
            // one structure for every line, so becoming current doesn't rebuild the view.
            TimelineView(.animation(paused: !(isActive && timing?.playing == true && !layered && (sweep || effects && (gapRow != nil || timing?.hold != nil))))) { _ in
                #if DEBUG
                let _ = isActive ? (VisualStats.lyricFrames += 1) : ()
                #endif
                let (mainTiming, backingTiming) = partTimings
                VStack(alignment: .leading, spacing: 0) {
                    if let rowHeight = gapRow {
                        GapMarker(state: LyricGapState(progress: timing?.progress() ?? 0, start: Double(timing?.start ?? 0) / 1000, seconds: Double(timing?.span ?? 0) / 1000),
                                  size: fontSize, visible: gap && !(layered && timing != nil))
                            .overlay { if layered, gap, let timing { LyricGapBars(timing: timing, size: fontSize).allowsHitTesting(false).transition(.identity) } }
                            .animation(nil, value: layered && gap)
                            .frame(height: rowHeight)
                    } else {
                        // an empty line still takes a line's height.
                        sweepText(mainText.isEmpty ? " " : mainText, size: mainSize, timing: mainTiming)
                    }
                    if !alone, let backing = line.backing {
                        sweepText(backing, size: fontSize * T.lyricBackingScale, timing: backingTiming)
                            .padding(.top, T.lyricBackingGap)
                            .fixedSize(horizontal: false, vertical: true)
                            .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { backingHeight = $0 }
                            .frame(height: showsBacking ? backingHeight : 0, alignment: .top)
                            .clipped()
                            .opacity(showsBacking ? 1 : 0)
                            .animation(T.easeOut(T.med), value: showsBacking)
                    }
                }
            }
            .multilineTextAlignment(.leading)
            .blur(radius: blur)
            .opacity(opacity)
            .animation(T.easeOut(T.med), value: distance)
            .animation(T.easeOut(T.fast), value: hovering)
            .offset(y: isActive ? 0 : fallOffset)
            .onChange(of: isActive, initial: true) { _, active in settle(active) }
            .task(id: isActive && takesLayers) {
                layered = false
                guard isActive, takesLayers else { return }
                try? await Task.sleep(for: .seconds(T.lyricLayerSettle))
                if !Task.isCancelled { layered = true }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
    }

    /// each part gets its share of the fill time by length, in text order; the last carries the held-word glow.
    private var partTimings: (main: LyricTiming?, backing: LyricTiming?) {
        guard let timing, !alone, let backing = line.backing else { return (timing, nil) }
        let mainCount = Double(max(1, line.main.count)), backingCount = Double(max(1, backing.count))
        let split = line.backingFirst ? backingCount / (mainCount + backingCount) : mainCount / (mainCount + backingCount)
        let first = timing.part(0, split, last: false), second = timing.part(split, 1, last: true)
        return line.backingFirst ? (second, first) : (first, second)
    }

    private func sweepText(_ text: String, size: CGFloat, timing: LyricTiming?) -> some View {
        // A blank line has nothing to fill, so it never takes the layers.
        let layered = layered && isActive && timing != nil && !text.trimmingCharacters(in: .whitespaces).isEmpty
        let hold = isActive && effects ? timing?.holdStrength() ?? 0 : 0
        let progress = timing?.progress() ?? 0
        var fillTiming = timing
        if !effects { fillTiming?.hold = nil }
        // the lift is 0 with effects or the sweep off and under Reduce Motion, and so is the wave.
        if lift(size) == 0 { fillTiming?.wave = nil }
        let wave = isActive ? (fillTiming?.waves(in: text) ?? []).map { size * CGFloat($0) } : []
        return LyricSweepRenderer.text(text, size: size)
            .textRenderer(LyricSweepRenderer(progress: progress, active: isActive, timed: timing?.line != nil,
                                             lifts: isActive && effects ? timing?.wordLifts(in: text) ?? [] : [], lift: lift(size), pixel: 1 / displayScale,
                                             hold: hold, lastWord: hold > 0 ? Self.lastWord(text) : 0, trailing: Self.trailing(text), wave: wave))
            // the renderer's progress is animatable: without this, the dimming animation would tween it into a sweep.
            .transaction { if !sweep { $0.animation = nil } }
            // the layers draw the line instead; the text stays for its place and its layout.
            .opacity(layered ? 0 : 1)
            // the swap is at once: the dimming animation mustn't fade one drawing over the other.
            .animation(nil, value: layered)
            // the whole width it was offered, so the layers' copy wraps the same way.
            .frame(maxWidth: .infinity, alignment: .leading)
            .overlayPreferenceValue(Text.LayoutKey.self) { layouts in
                if layered, let timing = fillTiming, let found = layouts.first {
                    GeometryReader { geo in
                        LyricFill(text: text, size: size, lines: LyricFillLine.lines(of: found.layout), origin: geo[found.origin], width: geo.size.width,
                                  timing: timing, lift: lift(size), lastWord: Self.lastWord(text), trailing: Self.trailing(text))
                    }
                    .allowsHitTesting(false)
                    .accessibilityHidden(true)
                    .transition(.identity)
                }
            }
            .animation(nil, value: layered)
    }

    /// trailing spaces left out.
    static func lastWord(_ text: String) -> Int {
        text.reversed().drop { $0.isWhitespace }.prefix { !$0.isWhitespace }.count
    }

    static func trailing(_ text: String) -> Int { text.reversed().prefix { $0.isWhitespace }.count }
}

/// bars loop on their own (not the music's level), light up as the gap passes, then swell and pop before the next
/// line moves up. under Reduce Motion they hold still and only light up and fade.
private struct GapMarker: View {
    let state: LyricGapState
    /// the marker scales with it.
    let size: CGFloat
    /// off, the marker has already popped while the next line moves up.
    let visible: Bool

    var body: some View {
        let count = T.lyricGapBars, bar = size * T.lyricGapWidth, quiet = size * T.lyricGapBarMin, full = size * T.lyricGapHeight
        let swell = state.swell
        HStack(alignment: .center, spacing: size * T.lyricGapBarGap) {
            ForEach(0..<count, id: \.self) { index in
                Capsule().fill(T.lyricGapUnlit.color)
                    .overlay { Capsule().fill(T.tOnArt.color).opacity(state.light(index)) }
                    .frame(width: bar, height: quiet + CGFloat(state.level(index)) * (full - quiet))
            }
        }
        // A fixed row, so the bars moving never move the lines around them.
        .frame(height: full)
        .shadow(color: T.lyricGlow[T.lyricGlow.count - 1].color.color, radius: bar * T.lyricGapGlow)
        .shadow(color: T.tOnArt.color.opacity(T.lyricGapSwellGlow * swell), radius: bar * T.lyricGapGlow * (1 + swell))
        .scaleEffect(CGFloat(state.scale))
        .opacity(visible ? state.opacity : 0)
    }
}

/// the bars' loops run on the song's time, so they hold still while it's paused.
struct LyricGapState {
    /// 0 to 1.
    let progress: Double
    /// seconds of the song.
    let start: Double
    /// seconds, so the swell and the pop land on its end.
    let seconds: Double

    private var reduced: Bool { Motion.isReduced }
    /// the pop ends `lyricGapPopLead` before the end; the swell runs just before the pop.
    private var left: Double { (1 - progress) * seconds }
    var swell: Double { Self.clamp((T.lyricGapPopLead + T.lyricGapPop + T.lyricGapSwell - left) / T.lyricGapSwell) }
    var pop: Double { Self.clamp((T.lyricGapPopLead + T.lyricGapPop - left) / T.lyricGapPop) }
    var scale: Double { reduced ? 1 : max(0, (1 + T.lyricGapSwellGrow * Self.easeOut(swell)) * (1 - Self.easeIn(pop))) }
    var opacity: Double { reduced ? 1 - pop : 1 }

    /// 0 to 1.
    func level(_ index: Int) -> Double {
        guard !reduced else { return T.lyricGapBarStill }
        let i = Double(index), now = start + progress * seconds
        let v = 0.5 + 0.5 * sin(now * (T.lyricGapBounceRate + i * T.lyricGapBounceSpread) + i * T.lyricGapBouncePhase)
        return v + (1 - v) * swell * swell
    }

    /// 0 to 1: off before its share of the gap, on after.
    func light(_ index: Int) -> Double {
        let t = Self.clamp(progress * Double(T.lyricGapBars) - Double(index))
        return t * t * (3 - 2 * t)
    }

    private static func clamp(_ x: Double) -> Double { max(0, min(1, x)) }
    private static func easeIn(_ x: Double) -> Double { x * x * x }
    private static func easeOut(_ x: Double) -> Double { 1 - pow(1 - x, 3) }
}

/// The queue in sections: the song playing, Next in Queue, Next from the source, then Autoplay. Rows move within
/// and between sections by press-and-hold dragging. Rows are built lazily, so a queue of any length is cheap.
struct QueueListView: View {
    let shell: ShellModel
    @State private var selection = RowSelection()
    @State private var drag = RowDrag(rowHeight: T.px(58))
    /// The Deezer card's measured height, so dragging knows where Autoplay's rows sit.
    @State private var deezerCardHeight: CGFloat = 0
    @Environment(\.isSnapshot) private var isSnapshot

    private var player: any PlayerViewModel { shell.player }
    /// Fixed, so dragging knows where the Autoplay rows sit.
    private var autoHeadHeight: CGFloat { T.px(66) }
    /// A section heading after the first, with the gap above it. Fixed, so dragging knows where its rows sit.
    private var sectionHeadHeight: CGFloat { T.queueSectionGap + T.px(39) }

    var body: some View {
        let content = VStack(alignment: .leading, spacing: 0) {
            if player.followingUser != nil, player.sessionOn {
                // a listening session: their song, then this side's session songs and its own queue waiting, to edit
                heading(L("Now Playing"))
                if let current = player.current { NowPlayingRow(song: current, shell: shell) }
            } else if let user = player.followingUser {
                // Listening along: the other person's song, and the listener's own queue waiting until following ends.
                heading(L("Now Playing"))
                if let current = player.current { NowPlayingRow(song: current, shell: shell) }
                EmptyStateView(symbol: "headphones", title: L("Listening along with {user}", ["user": user]),
                               hint: player.waitingSong.map { L("Your queue waits. When you stop, it carries on from “{song}”.", ["song": $0.title]) }
                                   ?? L("Songs come from what {user} plays.", ["user": user]), compact: true)
            } else if let station = player.station {
                EmptyStateView(symbol: "dot.radiowaves.left.and.right", title: L("Playing {name}", ["name": station.name]),
                               hint: L("A station has no next song. Your queue carries on when you play something from it."), compact: true)
            } else if let current = player.current {
                heading(L("Now Playing"))
                NowPlayingRow(song: current, shell: shell)
            }
            if player.followingUser == nil || player.sessionOn {
                if player.queuedCount > 0 || player.sessionOn {
                    sectionHead(gap: player.current != nil || player.station != nil) {
                        heading(player.sessionOn ? L("Session Queue") : L("Next in Queue"))
                        Spacer(minLength: 0)
                        HStack(spacing: T.queueHeadGap) {
                            headButton(Self.clearSymbol, L("Clear Queue")) { shell.clearQueued(); selection.clear() }
                            headButton("shuffle", L("Shuffle Queue"), disabled: player.queuedCount < 2) { player.shuffleQueued() }
                        }
                    }
                    if player.sessionOn { sessionNotes }
                    if player.queuedCount == 0 && player.upcomingCount > 0 { nextFromHead(gap: true) }
                } else if player.upcomingCount > 0 {
                    nextFromHead(gap: player.current != nil || player.station != nil)
                }
                // Only when nothing follows at all, never above Autoplay's songs or while it's finding them.
                if player.upcomingCount == 0 && player.station == nil && autoCount == 0 && player.autoplayShown != .finding {
                    EmptyStateView(symbol: "text.line.first.and.arrowtriangle.forward", title: L("Nothing queued"),
                                   hint: L("Add songs from any menu, or start an album or playlist."), compact: true,
                                   accessory: shell.player.autoplayEnabled ? nil : AnyView(Text(L("Autoplay is off.")).font(.navia(T.textSM)).foregroundStyle(T.t3.color)))
                }
                rows
                if player.autoplayShown == .repeating {
                    autoHeading(L("Off while Repeat is on, because the list never ends."), dimmed: true)
                }
            }
        }
        .padding(.horizontal, T.px(18))
        .padding(.top, T.px(8))
        .padding(.bottom, T.px(24))
        .foregroundStyle(T.tOnArt.color)

        Group {
            if isSnapshot {
                content.frame(maxHeight: .infinity, alignment: .top).clipped()
            } else {
                ScrollView(.vertical) { content }.scrollIndicators(.never)
            }
        }
        .mask(LinearGradient(stops: [.init(color: .black, location: 0.92), .init(color: .clear, location: 1)], startPoint: .top, endPoint: .bottom))
        // Songs dragged in from another list part the rows at the pointer.
        .onDrop(of: [.naviaSongs], delegate: QueueDropDelegate(shell: shell, drag: drag))
        .debugDropFrame(SongDropTarget.queue.key, shell: shell)
        .onAppear { player.setQueueVisible(true) }
        .onDisappear { drag.finish(drop: false); player.setQueueVisible(false) }
        #if DEBUG
        .onAppear { QueueDragDebug.current = drag }
        #endif
    }

    private var autoCount: Int { player.autoplayShown == .on || player.autoplayShown == .finding ? player.autoplayCount : 0 }

    /// A section's heading row, with the section gap above it when another section comes first.
    private func sectionHead(gap: Bool, @ViewBuilder content: () -> some View) -> some View {
        HStack(spacing: T.px(8)) { content() }
            .padding(.trailing, T.px(6))
            .padding(.top, gap ? T.queueSectionGap : 0)
    }

    /// the other side's songs can't be listed, only that they have some.
    @ViewBuilder private var sessionNotes: some View {
        let notes = player.sessionTheirs.map { L("{user} has songs queued", ["user": $0]) }
        ForEach(notes.isEmpty && player.queuedCount == 0 ? [L("Songs you add to the queue play in turns with the other side’s.")] : notes, id: \.self) { note in
            Text(note)
                .font(.navia(T.textSM))
                .foregroundStyle(T.tOnArt.color.opacity(0.7))
                .padding(.horizontal, T.px(8))
                .padding(.bottom, T.px(10))
        }
    }

    /// "Up Next" when the source isn't known; while following, the listener's own queue waiting.
    private func nextFromHead(gap: Bool) -> some View {
        sectionHead(gap: gap) {
            HStack(spacing: T.px(5)) {
                if player.followingUser != nil {
                    Text(player.waitingSong.map { L("Your queue waits at “{song}”", ["song": $0.title]) } ?? L("Your queue waits"))
                } else if let source = player.queueSource {
                    Text(L("Next from:")).fixedSize()
                    LinkedText(pieces: [LinkPiece(id: 0, text: source.title, route: source.link)], shell: shell)
                } else {
                    Text(L("Up Next"))
                }
            }
            .font(.navia(T.textLG, .semibold))
            .padding(.horizontal, T.px(8))
            .padding(.top, T.px(6))
            .padding(.bottom, T.px(10))
            Spacer(minLength: 0)
            if player.station == nil {
                HStack(spacing: T.queueHeadGap) {
                    headButton(Self.clearSymbol, player.queueSource.map { L("Clear {name}", ["name": $0.title]) } ?? L("Clear Up Next")) {
                        shell.clearList()
                        selection.clear()
                    }
                    ShuffleButton(shell: shell, size: T.queueHeadIcon, label: L("Shuffle"))
                }
            }
        }
    }

    /// SF Symbols has no broom, so this is the closest match.
    private static let clearSymbol = "eraser.line.dashed"

    /// A one-off action in a queue heading, such as Clear or Shuffle Queue.
    private func headButton(_ symbol: String, _ label: String, disabled: Bool = false, action: @escaping () -> Void) -> some View {
        ModeButton(symbol: symbol, label: label, size: T.queueHeadIcon, on: false, accent: shell.theme.accentSoft, onAccent: shell.theme.onAccent, disabled: disabled, action: action)
    }

    private func heading(_ text: String) -> some View {
        Text(text)
            .font(.navia(T.textLG, .semibold))
            .padding(.horizontal, T.px(8))
            .padding(.top, T.px(6))
            .padding(.bottom, T.px(10))
    }

    private func autoHeading(_ note: String?, dimmed: Bool) -> some View {
        VStack(alignment: .leading, spacing: T.px(2)) {
            Text(L("Autoplay")).font(.navia(T.textLG, .semibold)).opacity(dimmed ? 0.6 : 1)
            if let note { Text(note).font(.navia(T.textSM)).opacity(0.6).lineLimit(dimmed ? 2 : 1).truncationMode(.tail) }
        }
        .padding(.horizontal, T.px(8))
        .padding(.top, T.px(18))
        .padding(.bottom, T.px(10))
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// Up Next and Autoplay rows move only while the queue is what's playing. Autoplay songs dragged into Up Next
    /// become queued songs.
    private var canMove: Bool { player.station == nil && (player.upcomingCount > 1 || autoCount > 0) && !isSnapshot }
    /// Any row can be lifted, if only to drag it onto a playlist.
    private var canHold: Bool { player.station == nil && !isSnapshot }

    @ViewBuilder private var rows: some View {
        let count = player.upcomingCount
        let run = min(player.queuedCount, count)
        let autos = autoCount
        let _ = configureDrag(count: count, run: run, autos: autos, gap: autos > 0 ? autoHeadHeight + (asksDeezer ? deezerCardHeight : 0) : 0)
        let stack = Group {
            if isSnapshot {
                VStack(spacing: 0) {
                    ForEach(0..<min(run, 10), id: \.self) { row(at: $0) }
                    splitHead(run: run, count: count)
                    ForEach(run..<min(count, run + 10), id: \.self) { row(at: $0) }
                    autoplayPart(count: count, autos: min(autos, 10))
                }
            } else {
                LazyVStack(spacing: 0) {
                    ForEach(0..<run, id: \.self) { row(at: $0) }
                    splitHead(run: run, count: count)
                    ForEach(run..<count, id: \.self) { row(at: $0) }
                    autoplayPart(count: count, autos: autos)
                }
            }
        }
        stack
            .background(RowDragTracker(drag: drag))
            .background(RowDragSlot(drag: drag))
            .overlay(RowDragOverlay(drag: drag, accent: shell.theme.accent, onAccent: shell.theme.onAccent))
    }

    /// when Next in Queue shows above it.
    @ViewBuilder private func splitHead(run: Int, count: Int) -> some View {
        if run > 0 && run < count {
            nextFromHead(gap: true)
                .frame(height: sectionHeadHeight, alignment: .bottom)
                .offset(y: drag.splitShift)
        }
    }

    /// Autoplay's heading and songs. While songs are being found, placeholder rows stand in.
    @ViewBuilder private func autoplayPart(count: Int, autos: Int) -> some View {
        let finding = player.autoplayShown == .finding
        if autos > 0 || finding {
            autoHeading(finding ? L("Finding similar songs") : player.autoplayBasedOn.map { L("Based on {name}", ["name": $0]) }, dimmed: false)
                .frame(height: autoHeadHeight, alignment: .bottom)
                .offset(y: drag.followShift)
            if asksDeezer {
                deezerCard
                    .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { deezerCardHeight = $0 }
                    .offset(y: drag.followShift)
            }
            ForEach(0..<autos, id: \.self) { autoRow(at: $0, upNext: count) }
            if finding {
                ForEach(0..<T.queueFindingRows, id: \.self) { _ in placeholderRow }
            }
        }
    }

    /// Whether Autoplay opens with the Deezer question, for an account that never answered it.
    private var asksDeezer: Bool { shell.deezerChoice == nil && (shell.library as? CoreLibraryModel)?.core != nil }

    /// The Deezer question at the top of Autoplay. Either answer is saved, so it never returns.
    private var deezerCard: some View {
        PromptCard(title: L("Better Autoplay with Deezer?"), symbol: "paperplane", fact: L("Sends artist and song names to Deezer and ListenBrainz from this computer.")) {
            ActionButton(title: L("Not Now"), small: true, fill: true) { shell.setDeezerChoice(false) }
            ActionButton(title: L("Turn On"), kind: .primary(fill: shell.theme.accent, ink: shell.theme.onAccent), small: true, fill: true) { shell.setDeezerChoice(true) }
        }
        .padding(.bottom, T.promptCardGap)
    }

    /// A placeholder row while Autoplay finds its songs, at a song row's size so the songs replace it without a jump.
    private var placeholderRow: some View {
        HStack(spacing: T.px(11)) {
            RoundedRectangle(cornerRadius: T.rXS, style: .continuous).fill(T.onArtPlaceholder.color).frame(width: T.thumb, height: T.thumb)
            GeometryReader { geo in
                VStack(alignment: .leading, spacing: T.rowPlaceholderGap) {
                    RoundedRectangle(cornerRadius: T.rXS, style: .continuous).fill(T.onArtPlaceholder.color)
                        .frame(width: geo.size.width * T.rowPlaceholderTitleW, height: T.rowPlaceholderLine)
                    RoundedRectangle(cornerRadius: T.rXS, style: .continuous).fill(T.onArtPlaceholder.color)
                        .frame(width: geo.size.width * T.rowPlaceholderArtistW, height: T.rowPlaceholderLine)
                }
                .frame(maxHeight: .infinity, alignment: .center)
            }
        }
        .padding(.horizontal, T.px(8))
        .frame(height: T.px(58))
        .accessibilityHidden(true)
    }

    private func configureDrag(count: Int, run: Int, autos: Int, gap: CGFloat) {
        drag.count = count + autos
        drag.limit = autos > 0 ? count : nil
        drag.limitGap = gap
        // With both sections showing, the heading between them decides where drops go. With neither, dropped Autoplay
        // songs become queued songs.
        drag.split = run > 0 && run < count ? run : nil
        drag.splitGap = sectionHeadHeight
        drag.defaultIntoFirst = run == count
        drag.headAbove = run == 0 && count > 0 ? sectionHeadHeight : 0
        let player = player
        let selection = selection
        drag.onMove = { rows, before, intoFirst in
            player.moveInQueue(rows, before: before, queued: intoFirst)
            selection.clear()
        }
        // Dragged past the queue's sides, rows leave as songs that can drop on a playlist.
        let shell = shell
        drag.onLeave = { [weak drag] event, rows in
            let songs = rows.compactMap { index -> SongRow? in
                index < count ? player.upcoming(at: index)?.song : player.autoplay(at: index - count)?.song
            }
            guard let view = drag?.tracker else { return }
            SongDragSource.shared.begin(songs, fromQueue: true, event: event, in: view, shell: shell)
        }
    }

    @ViewBuilder private func row(at offset: Int) -> some View {
        if let entry = player.upcoming(at: offset) {
            let lifted = drag.pressed == offset
            QueueRow(entry: entry, offset: offset, shell: shell, selection: selection, movable: canMove && player.upcomingCount > 1, lifted: lifted, dragging: drag.isActive,
                     hold: canHold ? hold(offset) : nil, menu: { menuEntries(at: $0) },
                     play: { shell.player.jump(toUpcoming: offset) }, remove: removal([offset]), onDelete: { removeSelected() })
                .background { if lifted && drag.rows.count > 1 { StackedCard(tint: shell.player.current?.tintHex) } }
                .opacity(drag.isCarried(offset) ? 0 : 1)
                .offset(y: lifted ? drag.liftedOffset : drag.shift(offset))
                .zIndex(lifted ? 5 : 0)
        }
    }

    @ViewBuilder private func autoRow(at k: Int, upNext: Int) -> some View {
        if let entry = player.autoplay(at: k) {
            let offset = upNext + k
            let lifted = drag.pressed == offset
            QueueRow(entry: entry, offset: offset, shell: shell, selection: selection, movable: canMove, lifted: lifted, dragging: drag.isActive,
                     hold: canHold ? hold(offset) : nil, menu: { menuEntries(at: $0) },
                     play: { shell.player.playAutoplay(k) }, auto: true, remove: removal([offset]),
                     onDelete: { removeSelected() })
                .background { if lifted && drag.rows.count > 1 { StackedCard(tint: shell.player.current?.tintHex) } }
                .opacity(drag.isCarried(offset) ? 0 : 1)
                .offset(y: lifted ? drag.liftedOffset : drag.shift(offset))
                .zIndex(lifted ? 5 : 0)
        }
    }

    /// Removes rows given as offsets in Up Next followed by Autoplay. The playing song is never among them.
    private func remove(_ rows: [Int]) {
        guard player.station == nil else { return }
        let upNext = player.upcomingCount
        let up = rows.filter { $0 < upNext }
        let offered = rows.filter { $0 >= upNext }.map { $0 - upNext }
        // Autoplay's positions don't move when Up Next shrinks, so either order works.
        if !up.isEmpty { player.removeUpcoming(up) }
        if !offered.isEmpty { player.removeAutoplay(offered) }
        selection.clear()
    }

    private func removal(_ rows: [Int]) -> (() -> Void)? {
        player.station == nil && !isSnapshot ? { remove(rows) } : nil
    }

    private func removeSelected() {
        let rows = selection.selected.sorted()
        if !rows.isEmpty { remove(rows) }
    }

    /// Selected rows move together when the pressed one is among them. Otherwise just the pressed one moves.
    private func hold(_ offset: Int) -> RowHold {
        RowHold(deferClick: { [selection] in selection.selected.count > 1 && selection.selected.contains(offset) },
                lift: { [selection, drag] event in
                    let rows = selection.selected.count > 1 && selection.selected.contains(offset) ? selection.selected.sorted() : [offset]
                    if rows.count == 1 { selection.select([offset]) }
                    drag.lift(row: offset, rows: rows, at: event.locationInWindow)
                })
    }

    private func song(at index: Int) -> SongRow? {
        let upNext = player.upcomingCount
        return index < upNext ? player.upcoming(at: index)?.song : player.autoplay(at: index - upNext)?.song
    }

    private func menuEntries(at index: Int) -> [MenuEntry] {
        let canRemove = player.station == nil
        if selection.selected.count > 1, selection.selected.contains(index) {
            let rows = selection.selected.sorted()
            let songs = rows.compactMap { song(at: $0) }
            return SongMenu.several(songs, shell: shell, remove: canRemove ? SongMenu.removeFromQueue(count: rows.count) { remove(rows) } : nil)
        }
        guard let song = song(at: index) else { return [] }
        return SongMenu.single(song, shell: shell, remove: canRemove ? SongMenu.removeFromQueue(count: 1) { remove([index]) } : nil)
    }
}

/// The queue as a drop target for songs from another list. Rows part at the pointer. Songs dropped above the
/// Next from heading go into Next in Queue, and below it they join the list.
struct QueueDropDelegate: DropDelegate {
    let shell: ShellModel
    let drag: RowDrag

    private var takes: Bool { shell.songDrag.active && shell.songDrag.takes(.queue) && shell.player.current != nil && shell.player.station == nil }
    /// The drop location in window coordinates. DropInfo gives it from the top left of the queue's scroll view.
    private func point(_ info: DropInfo) -> NSPoint? {
        guard let scroll = drag.tracker?.enclosingScrollView else { return nil }
        let box = scroll.convert(scroll.bounds, to: nil)
        return NSPoint(x: box.minX + info.location.x, y: box.maxY - info.location.y)
    }

    func validateDrop(info: DropInfo) -> Bool { shell.songDrag.active && info.hasItemsConforming(to: [.naviaSongs]) }
    func dropEntered(info: DropInfo) {
        // The queue's own songs dropped back on it are ignored. Anything else it can't take now is refused.
        if !shell.songDrag.fromQueue { shell.songDrag.enter(.queue, taking: takes) }
        guard takes, let point = point(info) else { return }
        drag.takeIn(count: shell.songDrag.songs.count, at: point)
    }
    func dropUpdated(info: DropInfo) -> DropProposal? {
        guard takes else { return DropProposal(operation: .forbidden) }
        if let point = point(info) { drag.incoming == nil ? drag.takeIn(count: shell.songDrag.songs.count, at: point) : drag.moveIn(point) }
        return DropProposal(operation: .copy)
    }
    func dropExited(info: DropInfo) {
        shell.songDrag.exit(.queue)
        drag.endIn(drop: false)
    }
    func performDrop(info: DropInfo) -> Bool {
        shell.songDrag.exit(.queue)
        guard takes, let place = drag.endIn(drop: true) else { drag.endIn(drop: false); return false }
        let songs = shell.songDrag.songs
        shell.songDrag.end()
        shell.queueSongs(songs, at: place.before, queued: place.intoFirst)
        return true
    }
}

#if DEBUG
/// The queue's drag, for CoreCheck steps that drive it with window points.
@MainActor enum QueueDragDebug {
    static weak var current: RowDrag?
}
#endif

/// can't be dragged or clicked, but its artists and album are links.
private struct NowPlayingRow: View {
    let song: SongRow
    let shell: ShellModel

    var body: some View {
        HStack(spacing: T.px(12)) {
            ArtworkView(url: song.artworkURL, pixels: Int(T.thumb * 2))
                .frame(width: T.thumb, height: T.thumb)
                .clipShape(RoundedRectangle(cornerRadius: T.px(6), style: .continuous))
            VStack(alignment: .leading, spacing: 0) {
                TitledText(title: song.title, explicit: song.isExplicit, size: T.textMD, weight: .medium)
                LinkedText(pieces: LinkPiece.artist(song), shell: shell).font(.navia(T.textSM)).opacity(0.7)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Text(clock(Double(song.duration))).font(.navia(T.textSM).monospacedDigit()).opacity(0.65)
        }
        .padding(.horizontal, T.px(8))
        .frame(height: T.px(58))
        // The song's menu, without anything that removes or moves it.
        .rightClickMenu { SongMenu.single(song, shell: shell) }
    }
}

private struct QueueRow: View {
    let entry: QueueEntry
    let offset: Int
    let shell: ShellModel
    let selection: RowSelection
    let movable: Bool
    let lifted: Bool
    let dragging: Bool
    let hold: RowHold?
    let menu: (Int) -> [MenuEntry]
    let play: () -> Void
    /// Dimmed until hovered or selected.
    var auto = false
    /// Shows a remove button in place of the time on hover.
    var remove: (() -> Void)? = nil
    var onDelete: (() -> Void)? = nil
    @State private var hovering = false

    var body: some View {
        // `-naviaForceHover QueueRow:<offset>` draws a row hovered, for checks.
        let hovered = hovering || HoverDebug.forced("QueueRow:\(offset)")
        // On hover, a grip takes the time's place where rows can move. It stays on the lifted row.
        let grip = lifted || (movable && hovered && !dragging)
        let bright = !auto || hovered || lifted || selection.selected.contains(offset)
        SelectableRow(index: offset, selection: selection, height: T.px(58), radius: T.rMD,
                      hoverFill: dragging ? .clear : T.onArtHover.color, selectedFill: T.onArtSelected.color, leadWidth: T.thumb,
                      onPlay: { _ in play() }, menu: menu, onReturn: play,
                      leadPad: T.px(8), trailPad: T.px(8), hold: hold, onDelete: onDelete, flashID: entry.song.id) {
            ArtworkView(url: entry.song.artworkURL, pixels: Int(T.thumb * 2))
                .frame(width: T.thumb, height: T.thumb)
                .clipShape(RoundedRectangle(cornerRadius: T.px(6), style: .continuous))
                .opacity(bright ? 1 : 0.6)
                .allowsHitTesting(false)
        } text: {
            VStack(alignment: .leading, spacing: 0) {
                TitledText(title: entry.song.title, explicit: entry.song.isExplicit, size: T.textMD, weight: .medium).allowsHitTesting(false)
                LinkedText(pieces: LinkPiece.artist(entry.song), shell: shell) { selection.click(offset, modifiers: $0) }
                    .font(.navia(T.textSM)).opacity(0.7)
            }
            .opacity(bright ? 1 : 0.6)
            .frame(maxWidth: .infinity, alignment: .leading)
            if let remove, hovered, !dragging, !lifted {
                IconButton(symbol: "xmark", label: auto ? L("Remove {name} from Autoplay", ["name": entry.song.title]) : L("Remove {name} from Queue", ["name": entry.song.title]),
                           size: T.px(13), weight: .semibold, width: T.queueRemoveW) { remove() }
                    .help(auto ? L("Remove from Autoplay") : L("Remove from Queue"))
            }
            ZStack(alignment: .trailing) {
                Text(clock(Double(entry.song.duration))).font(.navia(T.textSM).monospacedDigit()).opacity(grip || (remove != nil && hovered && !dragging) ? 0 : bright ? 0.65 : 0.4)
                if grip {
                    Image(systemName: "line.3.horizontal")
                        .font(.system(size: T.px(15), weight: .semibold))
                        .opacity(lifted ? 0.9 : 0.6)
                        .frame(width: T.px(36))
                }
            }
            .allowsHitTesting(false)
        } trailing: {
            EmptyView()
        }
        .background {
            if lifted {
                RoundedRectangle(cornerRadius: T.rMD, style: .continuous)
                    .fill(Tint(hex: shell.player.current?.tintHex).vivid.mixed(with: RGBA(1, 1, 1), amount: 0.93).color)
                    .overlay(RoundedRectangle(cornerRadius: T.rMD, style: .continuous).strokeBorder(T.glassEdge.color, lineWidth: 1))
                    .shadow(T.shadowLift)
            }
        }
        .onHover { hovering = $0 }
    }
}

/// A second card peeking out under the lifted one when several rows move together.
private struct StackedCard: View {
    let tint: String?

    var body: some View {
        RoundedRectangle(cornerRadius: T.rMD, style: .continuous)
            .fill(Tint(hex: tint).vivid.mixed(with: RGBA(1, 1, 1), amount: 0.96).color.opacity(0.88))
            .overlay(RoundedRectangle(cornerRadius: T.rMD, style: .continuous).strokeBorder(T.dragCardUnderEdge.color, lineWidth: 1))
            .padding(.horizontal, T.px(8))
            .offset(y: T.px(6))
    }
}

/// Grows the player out of the dock to fill the window.
nonisolated struct ExpandClip: Shape {
    var progress: Double
    var animatableData: Double { get { progress } set { progress = newValue } }
    func path(in rect: CGRect) -> Path {
        let left = rect.width * 0.72 * (1 - progress)
        let radius = 22 * (1 - progress)
        return Path(roundedRect: CGRect(x: left, y: 0, width: rect.width - left, height: rect.height), cornerRadius: radius, style: .continuous)
    }
}

extension ExpandClip {
    static var animation: Animation? { T.ease(T.playerEnter) }
    /// out of the dock when docked; up from the bottom, and back down, when it's the bar.
    static func transition(fromDock: Bool) -> AnyTransition {
        let insertion = AnyTransition.modifier(active: ClipModifier(progress: 0, fromDock: fromDock), identity: ClipModifier(progress: 1, fromDock: fromDock))
        return .asymmetric(insertion: insertion, removal: fromDock ? .opacity : insertion)
    }
}

private struct ClipModifier: ViewModifier, Animatable {
    var progress: Double
    let fromDock: Bool
    var animatableData: Double { get { progress } set { progress = newValue } }
    func body(content: Content) -> some View {
        #if DEBUG
        MotionCheck.clipProgress = progress
        #endif
        return Group {
            if fromDock {
                content.clipShape(ExpandClip(progress: progress))
            } else {
                content.visualEffect { [progress] effect, proxy in effect.offset(y: proxy.size.height * (1 - progress)) }
            }
        }
    }
}

/// not observed.
@MainActor private final class ScrimCache {
    var controls: CGRect = .zero
    var alpha: Double?
}

struct PlayerExpanded: View {
    let shell: ShellModel
    let windowSize: CGSize
    @Environment(\.isSnapshot) private var isSnapshot
    private var player: any PlayerViewModel { shell.player }

    private var css: CGFloat { windowSize.width / T.scale }
    private var pad: CGFloat { T.px(min(132, max(36, css * 0.0825))) }
    private var gap: CGFloat { T.px(min(193, max(36, css * 0.12))) }
    private var narrow: Bool { css <= 820 }
    private var solo: Bool { shell.shownPlayerView == .artwork }
    /// The text scrim's inputs, the cover at `GlowScrim.grid` and where the controls sit.
    @State private var coverPixels: [UInt8]?
    /// kept out of view state: the rect changes every frame of a resize, and redrawing would rerun the blur estimate.
    @State private var scrim = ScrimCache()
    /// settled moves only (not during a live resize), so the scrim is worked out again for them.
    @State private var controlsMoved = 0
    private static let space = "expandedPlayer"

    var body: some View {
        let tint = Tint(hex: player.current?.tintHex)
        ZStack(alignment: .top) {
            LinearGradient(stops: GlowScrim.backdropStops(tint).map { .init(color: $0.color.color, location: $0.location) },
                           startPoint: .topLeading, endPoint: .bottomTrailing)

            ArtworkView(url: player.current?.artworkURL, kind: kind, pixels: 240)
                .frame(width: windowSize.width * 1.4, height: windowSize.height * 1.4)
                .blur(radius: T.playerGlowBlur)
                .saturation(T.playerGlowSaturation)
                .colorMultiply(Color(white: T.playerGlowBrightness))
                .opacity(T.playerGlowOpacity)
                .frame(width: windowSize.width, height: windowSize.height)
                .clipped()
                .allowsHitTesting(false)

            layout

            HStack {
                Spacer()
                FullScreenButton(shell: shell, size: T.px(20))
            }
            .foregroundStyle(T.tOnArt.color)
            .padding(.top, T.expandedTopRow)
            .padding(.trailing, T.px(32))

            // Esc closes the player.
            Button("") { close() }.keyboardShortcut(.cancelAction).opacity(0).allowsHitTesting(false).frame(width: 0, height: 0)
        }
        .foregroundStyle(tint.playerFg.color)
        .environment(\.playerDim, tint.playerFgDim.color)
        .environment(\.hoverInk, tint.playerFg.color)
        .environment(\.springPress, true)
        // the first click on an inactive window presses the control at once, as it activates the window.
        .allowsWindowActivationEvents(true)
        .coordinateSpace(.named(Self.space))
        .frame(width: windowSize.width, height: windowSize.height)
        .contentShape(Rectangle())
        .task(id: player.current?.artworkURL) {
            coverPixels = nil
            guard let url = player.current?.artworkURL else { return }
            // A cover the core hasn't fetched yet comes back nil at first, so retry with a backoff.
            var delay = 1.0
            while !Task.isCancelled {
                if let image = await ArtworkLoader.shared.load(url, pixels: 240) { coverPixels = GlowScrim.pixels(image); return }
                guard url.scheme == CoreArt.scheme else { return }
                try? await Task.sleep(for: .seconds(delay))
                delay = min(delay * 2, 30)
            }
        }
    }

    /// How dark the scrim behind the controls is. Zero unless the glow under them is too bright for 4.5:1 contrast.
    private func scrimAlpha(_ tint: Tint) -> Double {
        _ = controlsMoved
        guard let coverPixels else { return 0 }
        if shell.liveResizing, let last = scrim.alpha { return last }
        let alpha = GlowScrim.alpha(peak: GlowScrim.peak(cover: coverPixels, tint: tint, size: windowSize, rect: scrim.controls, blur: T.playerGlowBlur), tint: tint)
        scrim.alpha = alpha
        return alpha
    }

    private var kind: ArtworkKind { player.station == nil ? .album : .station }
    private func close() { shell.setExpanded(false) }

    private var artColumnWidth: CGFloat {
        let room = windowSize.width - 2 * pad - gap
        return min(T.px(solo ? 420 : 412), max(T.px(260), room))
    }

    @ViewBuilder private var layout: some View {
        if narrow {
            VStack(alignment: .leading, spacing: T.px(18)) {
                playerColumn(artFraction: solo ? 1 : 0.4)
                ViewSwitcher(shell: shell).frame(maxWidth: .infinity).padding(.bottom, T.px(14))
                if !solo { panel.frame(maxHeight: .infinity) }
            }
            .padding(.horizontal, T.px(26))
            .padding(.top, T.px(60))
            .padding(.bottom, T.px(20))
            .frame(maxHeight: .infinity, alignment: .top)
        } else if solo {
            ZStack(alignment: .top) {
                playerColumn(artFraction: 1)
                    .frame(width: artColumnWidth)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                ViewSwitcher(shell: shell)
                    .padding(.top, T.expandedTopRow)
            }
        } else {
            HStack(spacing: gap) {
                playerColumn(artFraction: 1).frame(width: artColumnWidth).frame(maxHeight: .infinity)
                ZStack(alignment: .top) {
                    panel
                        .padding(.top, T.expandedPanelTop)
                        .padding(.bottom, T.expandedEdge)
                    ViewSwitcher(shell: shell)
                        .frame(maxWidth: .infinity)
                        .padding(.top, T.expandedTopRow)
                }
            }
            .padding(.horizontal, pad)
        }
    }

    private var panel: some View {
        Swap(key: shell.shownPlayerView, kind: .view) {
            if shell.shownPlayerView == .lyrics {
                LyricsView(shell: shell, large: true, cssWidth: css)
            } else if shell.shownPlayerView == .queue {
                QueueListView(shell: shell)
            }
        }
    }

    private func playerColumn(artFraction: CGFloat) -> some View {
        VStack(spacing: 0) {
            GeometryReader { geo in
                let side = geo.size.width * artFraction
                ArtworkView(url: player.current?.artworkURL, kind: kind, animates: true)
                    .frame(width: side, height: side)
                    .clipShape(RoundedRectangle(cornerRadius: T.rLG, style: .continuous))
                    .shadow(T.shadowPlayerArt)
                    .scaleEffect(player.isPlaying ? 1 : T.coverPausedScale)
                    .animation(T.coverScale, value: player.isPlaying)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
            .aspectRatio(1 / artFraction, contentMode: .fit)
            PlayerControls(shell: shell, skipSize: T.px(30), playSize: T.px(44))
                .padding(.top, T.px(22))
                // The text scrim, black and blurred, reaching `glowScrimReach` past the sides and bottom.
                .background(alignment: .top) {
                    let alpha = scrimAlpha(Tint(hex: player.current?.tintHex))
                    if alpha > 0 {
                        RoundedRectangle(cornerRadius: T.glowScrimReach, style: .continuous)
                            .fill(.black.opacity(alpha))
                            .padding(.horizontal, -T.glowScrimReach)
                            .padding(.bottom, -T.glowScrimReach)
                            .blur(radius: T.glowScrimBlur)
                            .allowsHitTesting(false)
                    }
                }
                .onGeometryChange(for: CGRect.self, of: { $0.frame(in: .named(Self.space)) }) { [scrim] rect in
                    scrim.controls = rect
                    if !shell.liveResizing { controlsMoved += 1 }
                }
        }
    }
}
