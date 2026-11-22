import SwiftUI

struct HomePage: View {
    let shell: ShellModel

    var body: some View {
        let library = shell.library
        let essentials = library.essentialsArtists(limit: 12)
        let world = library.worldOf()
        let recent = library.recentAlbums(limit: 16)
        let top = library.topAlbums(limit: 16)
        let repeating = library.onRepeat()
        let because = library.becauseYouPlayed()
        let rotation = library.backInRotation()
        let stations = library.artistStations()
        let upcoming = library.upcomingHome()
        PageFrame(shell: shell, nav: .icons, refresh: .rows) {
            PageScroll {
                if essentials.isEmpty && world == nil && recent.isEmpty && top.isEmpty && repeating.isEmpty && because == nil && stations.isEmpty && rotation.isEmpty {
                    LibraryEmptyState(library: library, symbol: "music.note.list", title: L("Nothing here yet"), hint: L("Home fills in once the server has music."))
                } else {
                    if !stations.isEmpty {
                        Shelf(title: L("Artist Radio"), note: L("Non-stop music from the artists you've been playing")) {
                            ForEach(stations) { station in ArtistStationCard(station: station) { shell.startArtistRadio(station.seed) } }
                        }
                    }
                    SongShelf(title: L("On Repeat"), note: L("Last 4 weeks"), songs: repeating, shell: shell)
                    if let world, !world.albums.isEmpty {
                        Shelf(title: L("World of {name}", ["name": world.artist.name]), action: { shell.go(.artist(world.artist.id)) }) {
                            let shared = world.albums.sharedNames
                            ForEach(world.albums) { album in
                                AlbumCard(album: album, shell: shell, showEdition: shared.contains(album.name.lowercased())) { shell.go(.album(album.id)) }
                            }
                        }
                    }
                    if let because {
                        Shelf(title: L("Because you've been playing {name}", ["name": because.seed.name]), action: { shell.go(.artist(because.seed.id)) }) {
                            ForEach(because.artists) { artist in ArtistCard(artist: artist) { shell.go(.artist(artist.id)) } }
                        }
                    }
                    albumShelf(L("Recently Added"), recent) { shell.openAlbums(sort: .recent) }
                    UpcomingShelf(albums: upcoming, shell: shell)
                    if !essentials.isEmpty {
                        Shelf(title: L("Essentials"), action: { shell.go(.artists) }) {
                            ForEach(essentials) { artist in
                                PlayTile(artist: artist, band: L("Essentials"), title: L("{name} Essentials", ["name": artist.name]), subtitle: subtitle(artist)) {
                                    shell.player.play(library.essentialsSongs(for: artist), at: 0, shuffle: false)
                                }
                            }
                        }
                    }
                    albumShelf(L("Most Played"), top) { shell.go(.albums(.all)) }
                    if !rotation.isEmpty {
                        Shelf(title: L("Back in Rotation")) {
                            ForEach(rotation) { artist in ArtistCard(artist: artist) { shell.go(.artist(artist.id)) } }
                        }
                    }
                }
            }
        }
    }

    private func subtitle(_ artist: ArtistRow) -> String {
        let genre = shell.library.essentialsSongs(for: artist).first?.genre ?? ""
        return genre.isEmpty ? L("Music") : genre
    }

    @ViewBuilder private func albumShelf(_ title: String, _ albums: [AlbumRow], open: @escaping () -> Void) -> some View {
        if !albums.isEmpty {
            let shared = albums.sharedNames
            Shelf(title: title, action: open) {
                ForEach(albums) { album in
                    AlbumCard(album: album, shell: shell, showEdition: shared.contains(album.name.lowercased())) { shell.go(.album(album.id)) }
                }
            }
        }
    }
}
