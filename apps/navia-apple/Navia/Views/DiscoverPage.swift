import SwiftUI

struct DiscoverPage: View {
    let shell: ShellModel
    let tab: DiscoverTab
    @Environment(\.pageContext) private var ctx

    var body: some View {
        PageFrame(shell: shell, nav: .tabs(tab), refresh: tab == .forYou ? nil : .rows) {
            PageScroll { content }
        }
    }

    @ViewBuilder private var content: some View {
        switch tab {
        case .forYou: forYou
        case .new: new
        case .charts: charts
        case .radio: radio
        }
    }

    /// The core's For You rows in a fixed order, each hidden without data.
    @ViewBuilder private var forYou: some View {
        if let rows = shell.library.forYou() {
            if !rows.mixes.isEmpty {
                Shelf(title: L("Your Mixes")) { ForEach(rows.mixes) { mix in MixCard(mix: mix) { shell.go(.mix(mix.id)) } } }
            }
            albumShelf(L("New to You"), rows.newToYou)
            albumShelf(L("Unplayed from Your Top Artists"), rows.unplayed)
            SongShelf(title: L("Forgotten Favorites"), note: L("Not played in 6 months"), songs: rows.forgotten, shell: shell)
            albumShelf(L("On This Day"), rows.onThisDay, note: dayAndMonth(day: coreLocalDay()))
            albumShelf(L("Not Played Recently"), rows.notPlayedRecently)
            if rows.isEmpty {
                LibraryEmptyState(library: shell.library, symbol: "clock", title: L("Nothing here yet"), hint: L("These rows fill in once you've built up some play history."))
            }
        }
    }

    @ViewBuilder private func albumShelf(_ title: String, _ albums: [AlbumRow], note: String? = nil) -> some View {
        if !albums.isEmpty {
            let shared = albums.sharedNames
            Shelf(title: title, note: note) { ForEach(albums) { album in AlbumCard(album: album, shell: shell, showEdition: shared.contains(album.name.lowercased())) { shell.go(.album(album.id)) } } }
        }
    }

    @ViewBuilder private var new: some View {
        let albums = shell.library.recentAlbums(limit: 22)
        if albums.isEmpty {
            LibraryEmptyState(library: shell.library, symbol: "clock", title: L("Nothing new yet"), hint: L("Albums added to the server show up here."))
        } else {
            features(Array(albums.prefix(3)))
            let latest = shell.library.latestSongs(limit: 8)
            SongShelf(title: L("Latest Songs"), songs: latest, shell: shell, showAlbum: false)
            if albums.count > 3 { albumShelf(L("Latest Albums"), Array(albums.dropFirst(3))) }
        }
    }

    private func features(_ albums: [AlbumRow]) -> some View {
        let one = ctx.cssWidth <= 760
        return Group {
            if one {
                VStack(spacing: T.px(13)) { ForEach(albums) { feature($0) } }
            } else {
                HStack(alignment: .top, spacing: T.px(13)) { ForEach(albums) { feature($0).frame(maxWidth: .infinity) } }
            }
        }
        .padding(.horizontal, T.gutter)
    }

    private func feature(_ album: AlbumRow) -> some View {
        Button { shell.go(.album(album.id)) } label: {
            VStack(alignment: .leading, spacing: 0) {
                Text(L("NEW ALBUM")).font(.navia(T.textSM, .semibold)).tracking(0.03 * T.textSM).foregroundStyle(T.t2.color)
                Text(album.name).font(.navia(T.textFeature, .medium)).lineLimit(1).fullTextHelp(album.name).padding(.top, T.px(4))
                Text(album.artist).font(.navia(T.textFeature, .semibold)).foregroundStyle(T.t2.color).lineLimit(1).fullTextHelp(album.artist).padding(.bottom, T.px(14))
                ZStack {
                    (album.tintHex.flatMap(RGBA.init(hex:)) ?? T.tintFallback).color
                    ArtworkView(url: album.artworkURL, pixels: 200)
                        .scaleEffect(1.4)
                        .blur(radius: T.heroGlowBlur)
                        .saturation(T.heroGlowSaturation)
                        .opacity(T.heroGlowOpacity)
                    ArtworkView(url: album.artworkURL, pixels: 500)
                        .frame(width: T.px(284) * 0.8, height: T.px(284) * 0.8)
                        .clipShape(RoundedRectangle(cornerRadius: T.rSM, style: .continuous))
                        .artShadow()
                }
                .frame(height: T.px(284))
                .frame(maxWidth: .infinity)
                .clipShape(RoundedRectangle(cornerRadius: T.rMD, style: .continuous))
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    @ViewBuilder private var charts: some View {
        let songs = shell.library.topSongs(limit: 16)
        let albums = shell.library.topAlbums(limit: 16)
        let artists = shell.library.topArtists(limit: 18)
        SongShelf(title: L("Your Top Songs"), note: L("Last 6 months"), songs: songs, shell: shell)
        if !albums.isEmpty { albumShelf(L("Your Top Albums"), albums) }
        if !artists.isEmpty {
            Shelf(title: L("Your Top Artists")) { ForEach(artists) { artist in ArtistCard(artist: artist) { shell.go(.artist(artist.id)) } } }
        }
        if songs.isEmpty && albums.isEmpty && artists.isEmpty {
            LibraryEmptyState(library: shell.library, symbol: "clock", title: L("No play history yet"), hint: L("Once you've played some music, your most played shows up here."))
        }
    }

    @ViewBuilder private var radio: some View {
        let stations = shell.library.stations
        let artists = shell.library.radioArtists(limit: 14)
        if !stations.isEmpty {
            VStack(alignment: .leading, spacing: T.px(14)) {
                SectionTitle(title: L("Stations")).padding(.leading, T.gutter)
                HStack(spacing: T.cardGap) { ForEach(stations) { station in StationCard(shell: shell, station: station) } ; Spacer(minLength: 0) }
                    .padding(.horizontal, T.gutter)
            }
        }
        if !artists.isEmpty {
            Shelf(title: L("Song Radio")) {
                ForEach(artists) { artist in
                    PlayTile(artist: artist, band: L("Song Radio"), title: L("{name} Radio", ["name": artist.name]), subtitle: L("Similar artists")) {
                        shell.player.play(shell.library.similarSongs(for: artist), at: 0, shuffle: false)
                    }
                }
            }
        }
        if stations.isEmpty && artists.isEmpty {
            LibraryEmptyState(library: shell.library, symbol: "dot.radiowaves.left.and.right", title: L("No radio yet"), hint: L("Internet stations on the server show up here. Song radio needs similar-artist data from the server."))
        }
    }
}

private struct StationCard: View {
    let shell: ShellModel
    let station: RadioStation
    @State private var hovering = false

    var body: some View {
        let on = shell.player.station == station
        let playing = on && shell.player.isPlaying
        Button {
            if on { shell.player.togglePlayPause() } else { shell.player.playStation(station) }
        } label: {
            ZStack(alignment: .bottomLeading) {
                LinearGradient(colors: [shell.theme.accent.color, shell.theme.accent.mixed(with: RGBA(0, 0, 0), amount: 0.4).color], startPoint: .topLeading, endPoint: .bottomTrailing)
                VStack(alignment: .leading, spacing: 0) {
                    Text(station.name).font(.navia(T.textFeature, .semibold)).lineLimit(1).fullTextHelp(station.name)
                    Text(on ? (playing ? L("Playing now") : L("Paused")) : L("Internet radio")).font(.navia(T.textMD)).foregroundStyle(T.t2.color)
                }
                .padding(T.px(16))
                if hovering || on {
                    Image(systemName: playing ? "pause.fill" : "play.fill")
                        .font(.system(size: T.px(15)))
                        .contentTransition(Motion.isReduced ? .identity : .symbolEffect(.replace))
                        .frame(width: T.px(34), height: T.px(34))
                        .background(Circle().fill(T.hover.color))
                        .padding(T.px(14))
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing)
                }
            }
            .frame(width: T.px(300), height: T.px(150))
            .clipShape(RoundedRectangle(cornerRadius: T.rMD, style: .continuous))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .accessibilityLabel(playing ? L("Pause {title}", ["title": station.name]) : L("Play {title}", ["title": station.name]))
    }
}
