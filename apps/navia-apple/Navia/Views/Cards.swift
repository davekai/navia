import NaviaCoreFFI
import SwiftUI

private struct CardBody<Art: View>: View {
    let width: CGFloat
    let title: String
    var sub: String?
    var centered = false
    var explicit = false
    /// A subtitle with links, used instead of `sub`. A click outside the link text opens the card.
    var subLinks: [LinkPiece]? = nil
    var shell: ShellModel? = nil
    /// A kept playlist's or album's download mark, before the subtitle.
    var download: (kind: CollectionKind, id: String)? = nil
    @ViewBuilder var art: Art

    var body: some View {
        VStack(alignment: centered ? .center : .leading, spacing: 0) {
            art
            TitledText(title: title, explicit: explicit, size: T.textMD, weight: .medium).padding(.top, T.cardTextGap)
            HStack(spacing: T.downloadMarkGap) {
                if let download, let shell { DownloadMark(kind: download.kind, id: download.id, shell: shell) }
                if let subLinks, let shell, !subLinks.isEmpty {
                    LinkedText(pieces: subLinks, shell: shell).font(.navia(T.textMD)).foregroundStyle(T.t2.color)
                } else if let sub, !sub.isEmpty {
                    Text(sub).font(.navia(T.textMD)).foregroundStyle(T.t2.color).lineLimit(3).multilineTextAlignment(centered ? .center : .leading).fullTextHelp(sub)
                }
            }
        }
        .frame(width: width, alignment: centered ? .center : .leading)
        .contentShape(Rectangle())
    }
}

/// The plain card, lifted on hover with no pressed look.
private struct HoverCard<Label: View>: View {
    let action: () -> Void
    @ViewBuilder let label: (Bool) -> Label
    @State private var hovering = false

    var body: some View {
        Button(action: action) { label(hovering) }
            .buttonStyle(.plain)
            .onHover { hovering = $0 }
    }
}

struct AlbumCard: View {
    let album: AlbumRow
    var width: CGFloat = T.cardW
    var byArtist = false
    var shell: ShellModel? = nil
    /// Another album in the list has the same name, so the subtitle names this one's edition.
    var showEdition = false
    let open: () -> Void

    var body: some View {
        HoverCard(action: open) { hovering in
            let edition = showEdition ? album.edition : nil
            let parts: [String] = byArtist
                ? [album.year.map(String.init), edition, songCount(album.songCount)].compactMap { $0 }
                : [album.artist, album.year.map(String.init), edition].compactMap { $0 }
            // Each artist is its own link. The year stays plain.
            let links: [LinkPiece]? = byArtist ? nil
                : LinkPiece.joined([LinkPiece.artist(album), LinkPiece.plain(album.year.map(String.init) ?? ""), LinkPiece.plain(edition ?? "")], separator: " • ")
            CardBody(width: width, title: album.name, sub: parts.joined(separator: " • "), explicit: album.isExplicit, subLinks: links, shell: shell, download: (.album, album.id)) {
                CoverArt(url: album.artworkURL, width: width, label: album.isNewRelease ? L("New||release") : nil, lifted: hovering)
            }
        }
    }
}

/// `tag`: for lists shared with library albums.
struct UpcomingCard: View {
    let album: UpcomingAlbum
    var tag = false
    var width: CGFloat = T.cardW
    let open: () -> Void

    var body: some View {
        HoverCard(action: open) { hovering in
            TimelineView(.periodic(from: .now, by: 1)) { context in
                let left = TimeLeft(album, now: context.date)
                CardBody(width: width, title: album.title, sub: releaseLine(album, left: left, short: true), explicit: album.isExplicit) {
                    CoverArt(url: album.coverURL, width: width, label: tag ? left.tag : left.short, lifted: hovering)
                }
            }
        }
    }
}

private struct CoverArt: View {
    let url: URL?
    let width: CGFloat
    let label: String?
    let lifted: Bool

    var body: some View {
        ArtworkView(url: url, pixels: Int(width * 2))
            .frame(width: width, height: width)
            .clipShape(RoundedRectangle(cornerRadius: T.rMD, style: .continuous))
            .overlay(alignment: .bottomLeading) { if let label { CoverLabel(text: label).monospacedDigit() } }
    }
}

struct ArtistCard: View {
    let artist: ArtistRow
    var width: CGFloat = T.cardW
    let open: () -> Void

    var body: some View {
        HoverCard(action: open) { _ in
            CardBody(width: width, title: artist.name, centered: true) {
                ArtworkView(url: artist.photoURL, kind: .artist, pixels: Int(width * 2))
                    .frame(width: width, height: width)
                    .clipShape(Circle())
            }
        }
    }
}

struct PlaylistCard: View {
    let playlist: PlaylistRow
    var width: CGFloat = T.gridCard
    var shell: ShellModel? = nil
    let open: () -> Void

    var body: some View {
        HoverCard(action: open) { _ in
            CardBody(width: width, title: playlist.name, sub: playlistSubtitle(playlist), shell: shell, download: (.playlist, playlist.id)) {
                PlaylistCover(playlist: playlist).frame(width: width, height: width)
            }
        }
    }
}

struct MixCard: View {
    let mix: MixInfo
    var width: CGFloat = T.cardW
    let open: () -> Void

    var body: some View {
        HoverCard(action: open) { hovering in
            CardBody(width: width, title: mix.title, sub: mix.artistsLine) {
                PlaylistCover(mix: mix)
                    .frame(width: width, height: width)
            }
        }
    }
}

struct CoverLabel: View {
    enum Style { case chip, mark(symbol: String, width: CGFloat) }
    let text: String
    var style: Style = .chip

    var body: some View {
        switch style {
        case .chip:
            Text(text)
                .font(.navia(T.textXS, .semibold))
                .foregroundStyle(T.tOnArt.color)
                .padding(.horizontal, T.coverLabelPadX)
                .padding(.vertical, T.coverLabelPadY)
                .background(Capsule().fill(T.scrim.color))
                .overlay(Capsule().strokeBorder(T.glassEdge.color, lineWidth: 1))
                .padding(T.coverLabelInset)
        case let .mark(symbol, width):
            let size = width * T.coverMarkText, glyph = width * T.coverMarkGlyph
            HStack(spacing: width * T.coverMarkGap) {
                Image(systemName: symbol).resizable().scaledToFit().frame(width: glyph, height: glyph)
                Text(text).font(.navia(size, .semibold)).tracking(T.coverMarkTracking * size).textCase(.uppercase)
            }
            .foregroundStyle(T.coverMarkInk.color)
            .padding(width * T.coverMarkInset)
        }
    }
}

func playlistSubtitle(_ playlist: PlaylistRow) -> String {
    playlist.isMine || playlist.ownerName.isEmpty ? songCount(playlist.songCount) : "\(songCount(playlist.songCount)) • \(L("By {name}", ["name": playlist.ownerName]))"
}

/// A tile with light text on a tint, such as genre tiles and Artist Radio. It shows `T.panel2` until the tint is
/// known, then the tint darkened enough for light text, as a gradient along `tileFillAngle`.
struct TileFill: View {
    let tint: Tint?
    let width: CGFloat
    let height: CGFloat

    var body: some View {
        if let vivid = tint?.playerVivid {
            let (l, c, h) = OKLCH.from(vivid)
            let shade = OKLCH.rgb(l: l * T.tileFillShade, c: c, h: h)
            // Like CSS's gradient line, through the center at the angle and long enough that the corners land on 0% and 100%.
            let a = T.tileFillAngle * .pi / 180
            let (dx, dy) = (sin(a), -cos(a))
            let half = (abs(width * dx) + abs(height * dy)) / 2
            LinearGradient(colors: [vivid.color, shade.color],
                           startPoint: UnitPoint(x: 0.5 - dx * half / width, y: 0.5 - dy * half / height),
                           endPoint: UnitPoint(x: 0.5 + dx * half / width, y: 0.5 + dy * half / height))
                .transition(.opacity)
        } else {
            T.panel2.color
        }
    }
}

/// One Artist Radio station on Home, with the seed artist and its neighbors that have photos.
struct ArtistStation: Identifiable, Equatable, Sendable {
    let seed: ArtistRow
    let neighbours: [ArtistRow]
    var id: String { seed.id }

    /// "With A, B, C and more", naming up to three neighbors. Empty when there are none.
    var line: String {
        guard !neighbours.isEmpty else { return "" }
        let names = neighbours.prefix(3).map(\.name).joined(separator: ", ")
        return neighbours.count > 3 ? L("With {names} and more", ["names": names]) : L("With {names}", ["names": names])
    }
}

/// An Artist Radio tile on Home, on the tint of the seed's photo. The seed's round photo sits in front of up to two
/// neighbors, which spread on hover. The name shrinks, then truncates, to fit. A click starts the seed's radio.
struct ArtistStationCard: View {
    let station: ArtistStation
    var width: CGFloat = T.cardW
    let play: () -> Void
    @State private var hex: String?

    var body: some View {
        HoverCard(action: play) { hovering in
            VStack(alignment: .leading, spacing: 0) {
                tile(hovering: hovering)
                if !station.line.isEmpty {
                    Text(station.line).font(.navia(T.textMD)).foregroundStyle(T.t2.color).lineLimit(2).fixedSize(horizontal: false, vertical: true).padding(.top, T.cardTextGap)
                }
            }
            .frame(width: width, alignment: .leading)
            .contentShape(Rectangle())
        }
        .accessibilityLabel(L("Play {name}", ["name": L("{name} Radio", ["name": station.seed.name])]))
        .task(id: station.seed.photoURL) {
            guard let url = station.seed.photoURL else { return }
            let source = PlaylistCoverSource.image(url)
            if let known = PlaylistTints.shared.cached(source) { hex = known } else { hex = await PlaylistTints.shared.read(source) }
        }
    }

    private func tile(hovering: Bool) -> some View {
        let tint = hex.map { Tint(hex: $0) }
        let inset = hovering ? width * T.stationInset : 0
        let ink = (tint?.playerVivid ?? T.panel2).mixed(with: RGBA(0, 0, 0), amount: T.stationShade)
        let pad = width * T.stationPad
        return ZStack(alignment: .bottomLeading) {
            TileFill(tint: tint, width: width, height: width)
            ZStack {
                ArtworkView(url: station.seed.photoURL, kind: .artist, pixels: Int(width * 2))
                LinearGradient(stops: [.init(color: ink.color, location: 0), .init(color: ink.color.opacity(0), location: T.stationShadeEnd)],
                               startPoint: .bottom, endPoint: .top)
                LinearGradient(stops: [.init(color: T.scrim.color, location: 0), .init(color: T.scrim.color.opacity(0), location: T.stationMarkShadeEnd)],
                               startPoint: .top, endPoint: .bottom)
            }
            .frame(width: width - inset * 2, height: width - inset * 2)
            .clipShape(RoundedRectangle(cornerRadius: hovering ? inset : T.rMD, style: .continuous))
            .frame(width: width, height: width)
            if !station.neighbours.isEmpty { faces(hovering: hovering).padding(.leading, pad).padding(.bottom, width * T.stationFacesBottom) }
            Text(station.seed.name).font(.navia(width * T.stationName, .bold)).tracking(-0.01 * width * T.stationName)
                .foregroundStyle(T.tEm.color).lineLimit(1).truncationMode(.tail).fullTextHelp(station.seed.name)
                .frame(width: width - pad * 2, alignment: .leading)
                .padding(.leading, pad).padding(.bottom, width * T.stationNameBottom)
        }
        .frame(width: width, height: width)
        .overlay(alignment: .topLeading) { CoverLabel(text: L("Radio"), style: .mark(symbol: "dot.radiowaves.left.and.right", width: width)) }
        .clipShape(RoundedRectangle(cornerRadius: T.rMD, style: .continuous))
        .animation(T.ease(T.med), value: hovering)
    }

    /// each face grows from its bottom-left corner; a later one also shifts right by its place times the spread.
    private func faces(hovering: Bool) -> some View {
        let face = width * T.stationFace, ring = width * T.stationRing
        return HStack(spacing: width * T.stationFaceGap) {
            ForEach(Array(station.neighbours.prefix(3).enumerated()), id: \.element.id) { index, n in
                ArtworkView(url: n.photoURL, kind: .artist, pixels: Int(face * T.stationFaceHover * 2))
                    .frame(width: face, height: face)
                    .clipShape(Circle())
                    .background(Circle().fill(T.stationRingInk.color).padding(-ring))
                    .scaleEffect(hovering ? T.stationFaceHover : 1, anchor: .bottomLeading)
                    .offset(x: hovering ? CGFloat(index) * width * T.stationFaceSpread : 0)
            }
        }
    }
}

struct PlayTile: View {
    let artist: ArtistRow
    let band: String
    let title: String
    let subtitle: String
    var width: CGFloat = T.cardW
    let play: () -> Void

    var body: some View {
        HoverCard(action: play) { hovering in
            let tint = artist.tintHex.flatMap(RGBA.init(hex:)) ?? RGBA(0.33, 0.33, 0.33)
            CardBody(width: width, title: title, sub: subtitle) {
                ZStack(alignment: .top) {
                    tint.color
                    VStack(spacing: 0) {
                        ZStack(alignment: .bottomLeading) {
                            tint.mixed(with: RGBA(hex: "c9c9c9")!, amount: 0.32).color
                            Text(band).font(.system(size: width * 0.136, weight: .semibold)).tracking(-0.025 * width * 0.136)
                                .foregroundStyle(Color(red: 0.067, green: 0.067, blue: 0.067))
                                .padding(.horizontal, width * 0.036)
                                .padding(.bottom, width * 0.03)
                        }
                        .frame(height: width * 0.31)
                        ArtworkView(url: artist.photoURL, kind: .artist, pixels: Int(width * 2), focusY: 0.2)
                    }
                }
                .frame(width: width, height: width)
                .clipShape(RoundedRectangle(cornerRadius: T.rMD, style: .continuous))
            }
        }
        .accessibilityLabel(L("Play {title}", ["title": title]))
    }
}

/// an image set in `Assets.xcassets`, sized for page headers.
enum BundledCover {
    case likedSongs
    var asset: String { "LikedSongs" }
}

struct PlaylistCover: View {
    var collage: [URL] = []
    var coverURL: URL? = nil
    var small = false
    /// A quiet corner mark for a list the app made, such as "Mix". Hidden at the small size.
    var label: String? = nil
    /// Navia's own cover from the asset catalog, in place of the server's.
    var bundled: BundledCover? = nil
    /// plays an animated server image (the page header).
    var animates = false
    /// fills the frame it's given, cells cropped at `focusY`, square-cornered and unmarked: a sheet's cover band.
    var band = false

    /// Navia's bundled cover, so it's the same on every server.
    static var likedSongs: PlaylistCover {
        var cover = PlaylistCover()
        cover.bundled = .likedSongs
        return cover
    }

    private init() {}

    /// its albums' grid with the "Mix" chip.
    init(mix: MixInfo) {
        collage = mix.collage
        label = L("Mix")
    }

    init(playlist: PlaylistRow, small: Bool = false, animates: Bool = false) {
        collage = playlist.collage
        coverURL = playlist.coverURL
        self.small = small
        self.animates = animates
    }

    init(ref: PlaylistRef, small: Bool = false) {
        collage = ref.collage
        coverURL = ref.coverURL
        self.small = small
    }

    var body: some View {
        if band {
            GeometryReader { geo in
                serverCover(geo.size)
                    .frame(width: geo.size.width, height: geo.size.height, alignment: .topLeading)
                    .clipped()
            }
        } else {
            GeometryReader { geo in
                let side = geo.size.width
                serverCover(CGSize(width: side, height: side))
                    .frame(width: side, height: side, alignment: .topLeading)
                    .overlay(alignment: .bottomLeading) {
                        if let label, !small { CoverLabel(text: label) }
                    }
                    .clipShape(RoundedRectangle(cornerRadius: small ? T.sidebarCoverR : T.rMD, style: .continuous))
            }
            .aspectRatio(1, contentMode: .fit)
        }
    }

    func asBand() -> PlaylistCover {
        var cover = self
        cover.band = true
        return cover
    }

    private var focusY: CGFloat? { band ? T.sheetCoverFocusY : nil }

    /// the server's image, else the album grid, else a plain placeholder.
    @ViewBuilder private func serverCover(_ size: CGSize) -> some View {
        let pixels = Int(max(size.width, size.height) * 2)
        if let bundled {
            // pre-sized for the page header, decoded once and cached by the system.
            Image(bundled.asset).resizable().interpolation(.high).aspectRatio(contentMode: .fill).frame(width: size.width, height: size.height).clipped()
        } else if let coverURL {
            ArtworkView(url: coverURL, pixels: pixels, focusY: focusY, animates: animates).frame(width: size.width, height: size.height)
        } else if collage.count == 1 {
            ArtworkView(url: collage[0], pixels: pixels, focusY: focusY).frame(width: size.width, height: size.height)
        } else if !collage.isEmpty {
            collageGrid(size)
        } else {
            ArtworkView(url: nil).frame(width: size.width, height: size.height)
        }
    }

    /// Cells in reading order, repeating so none is empty, as Navidrome tiles its own.
    private var cells: [URL] { Self.gridCells(collage) }

    nonisolated static func gridCells<Tile>(_ tiles: [Tile]) -> [Tile] {
        switch tiles.count {
        case 1: [tiles[0], tiles[0], tiles[0], tiles[0]]
        case 2: [tiles[0], tiles[1], tiles[1], tiles[0]]
        case 3: [tiles[0], tiles[1], tiles[2], tiles[0]]
        default: Array(tiles.prefix(4))
        }
    }

    private func collageGrid(_ size: CGSize) -> some View {
        VStack(spacing: 0) {
            ForEach(0..<2, id: \.self) { row in
                HStack(spacing: 0) {
                    ForEach(0..<2, id: \.self) { col in
                        ArtworkView(url: cells[row * 2 + col], pixels: Int(max(size.width, size.height)), focusY: focusY).frame(width: size.width / 2, height: size.height / 2)
                    }
                }
            }
        }
    }
}
