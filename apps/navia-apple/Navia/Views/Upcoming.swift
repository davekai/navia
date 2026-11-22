import SwiftUI

nonisolated struct TimeLeft {
    let out: Bool
    /// whole local days.
    let daysAgo: Int
    let days: Int
    let hours: Int
    let minutes: Int
    let seconds: Int

    init(_ album: UpcomingAlbum, now: Date) {
        let left = max(0, Int(album.releaseMoment.timeIntervalSince(now).rounded(.down)))
        out = left == 0
        days = left / 86_400
        hours = left % 86_400 / 3_600
        minutes = left % 3_600 / 60
        seconds = left % 60
        let calendar = Calendar.current
        daysAgo = calendar.dateComponents([.day], from: calendar.startOfDay(for: album.releaseMoment), to: calendar.startOfDay(for: now)).day ?? 0
    }

    var short: String {
        if out { return L("Out now") }
        let clock = String(format: "%02d:%02d:%02d", hours, minutes, seconds)
        return days > 0 ? L("{days}d {clock}", ["days": String(days), "clock": clock]) : clock
    }

    /// for lists shared with library albums.
    var tag: String { out ? L("Out now") : L("Upcoming") }
}

func releaseLine(_ album: UpcomingAlbum, left: TimeLeft, short: Bool) -> String {
    let date: String
    if short {
        date = fmtDate(ISO8601DateFormatter().string(from: album.releaseMoment))
    } else {
        let parts = album.releaseDate.split(separator: "-").compactMap { Int($0) }
        guard parts.count == 3 else { return "" }
        date = fmtRelease(year: parts[0], month: parts[1], day: parts[2])
    }
    switch (short, left.out) {
    case (true, false): return L("Releases {date}", ["date": date])
    case (true, true): return L("Released {date}", ["date": date])
    case (false, false): return L("Releases on {date}", ["date": date])
    case (false, true): return L("Released on {date}", ["date": date])
    }
}

struct UpcomingShelf: View {
    let albums: [UpcomingAlbum]
    let shell: ShellModel

    var body: some View {
        if !albums.isEmpty {
            Shelf(title: L("Upcoming")) {
                ForEach(albums) { album in UpcomingCard(album: album) { shell.go(.upcoming(album.id)) } }
            }
        }
    }
}

struct Countdown: View {
    let left: TimeLeft
    let labelColor: RGBA

    var body: some View {
        HStack(spacing: 0) {
            if left.out {
                cell(L("Out now"), left.daysAgo > 0 ? Ln("Released {count} day ago", "Released {count} days ago", left.daysAgo) : L("Released today"))
                    .padding(.horizontal, T.countdownPadX)
            } else {
                let parts = [(left.days, L("Days")), (left.hours, L("Hours")), (left.minutes, L("Minutes")), (left.seconds, L("Seconds"))]
                ForEach(parts.indices, id: \.self) { i in
                    if i > 0 { Rectangle().fill(T.countdownDivider.color).frame(width: 1) }
                    cell(String(format: "%02d", parts[i].0), parts[i].1).frame(width: T.countdownCellW)
                }
            }
        }
        .fixedSize()
        .padding(.vertical, T.countdownPadY)
        .background(RoundedRectangle(cornerRadius: T.countdownR, style: .continuous).fill(T.countdownFill.color))
        .overlay(RoundedRectangle(cornerRadius: T.countdownR, style: .continuous).strokeBorder(T.glassEdge.color, lineWidth: 1))
        .accessibilityElement(children: .combine)
        .accessibilityLabel(left.out ? L("Out now") : L("Time until release"))
    }

    private func cell(_ number: String, _ label: String) -> some View {
        VStack(spacing: T.countdownLabelGap) {
            Text(number).font(.navia(T.countdownNumber, .semibold)).monospacedDigit().tracking(-0.02 * T.countdownNumber).foregroundStyle(T.tEm.color).lineLimit(1)
            Text(label).font(.navia(T.countdownLabel, .medium)).foregroundStyle(labelColor.color)
        }
    }
}

struct UpcomingPage: View {
    let shell: ShellModel
    let id: UInt64

    var body: some View {
        let state = shell.library.upcomingPage(id)
        PageFrame(shell: shell, nav: .icons, edgeFade: true) {
            switch state {
            case .upcoming(let album): UpcomingDetail(shell: shell, album: album).id(album.id)
            case .loading, .onServer: Color.clear
            case .missing: PageScroll { EmptyStateView(symbol: "square.stack", title: L("This album isn't in your library")) }
            }
        }
        .onChange(of: state, initial: true) { _, now in
            if case .onServer(let albumID) = now { shell.replace(.album(albumID)) }
        }
    }
}

private struct UpcomingDetail: View {
    let shell: ShellModel
    let album: UpcomingAlbum
    @State private var holder = Once<any SongsViewModel>()
    @State private var tintHex: String?
    @Environment(\.pageContext) private var ctx

    var body: some View {
        let songs = holder.get { shell.library.songs(forUpcoming: album) }
        let tint = Tint(hex: tintHex)
        let owned = album.tracks.count - album.unavailable.count
        let note = [L("{listed} of {total} songs announced so far", ["listed": String(album.tracks.count), "total": String(max(album.total, album.tracks.count))]),
                    owned > 0 ? L("{count} in the library", ["count": String(owned)]) : ""].filter { !$0.isEmpty }.joined(separator: " • ")
        let header = VStack(alignment: .leading, spacing: T.albumSectionGap) {
            AlbumHead(artworkURL: album.coverURL, stacked: ctx.cssWidth <= 620) {
                Text(L("Album")).font(.navia(T.textMD, .semibold)).foregroundStyle(tint.fgDim.color).padding(.bottom, T.countdownLabelGap)
                AlbumTitle(name: album.title, explicit: album.isExplicit)
                AlbumArtistLine(name: album.artist, artistID: libraryArtist, color: tint.fg, shell: shell).padding(.top, T.albumArtistGap)
                TimelineView(.periodic(from: .now, by: 1)) { context in
                    let left = TimeLeft(album, now: context.date)
                    VStack(alignment: .leading, spacing: 0) {
                        Text(releaseLine(album, left: left, short: false)).font(.navia(T.textMD, .medium)).foregroundStyle(tint.fgDim.color).padding(.top, T.albumMetaGap)
                        Countdown(left: left, labelColor: tint.fgDim).padding(.top, T.countdownGap)
                    }
                }
            }
            SectionHeading(title: L("Tracklist preview"), note: note)
        }
        .foregroundStyle(T.t1.color)
        .padding(.leading, ctx.sidebarUnder + ctx.gutter)
        .padding(.trailing, ctx.end + ctx.gutter + ctx.dockUnder)
        .padding(.top, PagePadding.top)
        .padding(.bottom, T.shelfHeadGap)
        .background(alignment: .top) { PageTint(hex: tintHex ?? "2a2b33") }
        SongsTable(songs: songs, shell: shell, revision: songs.revision, editRevision: songs.editRevision,
                   nowPlayingID: shell.player.current?.id, variant: .album, pageHeader: AnyView(header),
                   bottomInset: PagePadding.bottom(hasBar: ctx.hasBar), unavailable: album.unavailable)
            .task(id: album.coverURL) {
                guard let url = album.coverURL else { return }
                tintHex = await PlaylistTints.shared.read(.image(url))
            }
    }

    /// only when the artist it was found for has the album artist's name.
    private var libraryArtist: String? {
        shell.library.artist(id: album.artistID).flatMap { $0.name.localizedCaseInsensitiveCompare(album.artist) == .orderedSame ? $0.id : nil }
    }
}
