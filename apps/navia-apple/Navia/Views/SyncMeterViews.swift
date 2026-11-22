import NaviaCoreFFI
import SwiftUI

/// A sync's progress, shown under the empty state while the page has nothing yet, otherwise as a card at the
/// sidebar's foot. A sync that isn't the first shows only after `T.syncShowAfter`, so quick refreshes stay silent.

/// "1,240 of 3,812 albums", or a running count when the server gave no total. Nil before the first page.
/// The bar runs across the whole sync when the server gave every total.
func syncDetail(_ p: SyncProgressRow) -> String? {
    guard let stage = p.stage else { return nil }
    let done = fmtInt(Int(p.done))
    switch (stage, p.total) {
    case (.artists, let total?): return Ln("{done} of {count} artist", "{done} of {count} artists", Int(total), ["done": done])
    case (.albums, let total?): return Ln("{done} of {count} album", "{done} of {count} albums", Int(total), ["done": done])
    case (.songs, let total?): return Ln("{done} of {count} song", "{done} of {count} songs", Int(total), ["done": done])
    case (.playlists, let total?): return Ln("{done} of {count} playlist", "{done} of {count} playlists", Int(total), ["done": done])
    // No total from the server, so count up.
    case _ where p.done == 0: return nil
    case (.artists, _): return Ln("{count} artist", "{count} artists", Int(p.done))
    case (.albums, _): return Ln("{count} album", "{count} albums", Int(p.done))
    case (.songs, nil): return Ln("{count} song", "{count} songs", Int(p.done))
    case (.playlists, nil): return Ln("{count} playlist", "{count} playlists", Int(p.done))
    }
}

/// Filled to `fraction`, or without one a sweeping segment, which runs even under Reduce Motion.
struct SyncBar: View {
    let fraction: Double?
    let fill: RGBA

    var body: some View {
        GeometryReader { geo in
            let w = geo.size.width
            ZStack(alignment: .leading) {
                Capsule().fill(T.syncTrack.color)
                if let fraction {
                    Capsule().fill(fill.color).frame(width: w * fraction)
                } else if Motion.shared.reduced {
                    Capsule().fill(fill.color).frame(width: w * T.syncSweepSpan).offset(x: w * (1 - T.syncSweepSpan) / 2)
                } else {
                    TimelineView(.animation) { context in
                        let t = context.date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: T.syncSweep) / T.syncSweep
                        Capsule().fill(fill.color).frame(width: w * T.syncSweepSpan)
                            .offset(x: w * (-T.syncSweepSpan + t * (1 + T.syncSweepSpan)))
                    }
                }
            }
            .clipShape(Capsule())
        }
        .frame(height: T.syncBarH)
    }
}

/// The count and bar under the empty state's "Syncing your library".
struct SyncPageMeter: View {
    let library: any LibraryViewModel
    @Environment(\.theme) private var theme

    var body: some View {
        if let p = library.syncProgress, p.stage != nil {
            VStack(spacing: T.px(8)) {
                if let detail = syncDetail(p) {
                    Text(detail).font(.navia(T.textXS)).foregroundStyle(T.t2.color)
                }
                SyncBar(fraction: p.fraction.map(Double.init), fill: theme?.syncFill ?? Theme.defaultAccent)
            }
            .frame(width: T.syncMeterW)
            .onAppear { library.syncMeterVisible(true) }
            .onDisappear { library.syncMeterVisible(false) }
        }
    }
}

/// The card at the sidebar's foot, shown whenever the page isn't showing the meter.
struct SyncCard: View {
    let progress: SyncProgressRow
    let fill: RGBA

    var body: some View {
        let p = progress
        VStack(alignment: .leading, spacing: T.px(8)) {
            VStack(alignment: .leading, spacing: 0) {
                Text(L("Syncing library")).font(.navia(T.textMD, .medium)).lineLimit(1)
                if let detail = syncDetail(p) {
                    Text(detail).font(.navia(T.textXS)).foregroundStyle(T.t2.color).lineLimit(1)
                }
            }
            SyncBar(fraction: p.fraction.map(Double.init), fill: fill)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, T.px(10))
        .padding(.vertical, T.px(10))
        .background(T.row.color, in: RoundedRectangle(cornerRadius: T.rMD, style: .continuous))
    }
}
