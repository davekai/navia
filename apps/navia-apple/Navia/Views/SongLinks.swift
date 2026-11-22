import AppKit
import SwiftUI

/// Artist, album and genre text in song rows are links. Hovering the text underlines it, and a click opens the page.
/// Each credited artist is its own link. Anything without a page stays plain text.
nonisolated enum SongLinks {
    static func artists(_ song: SongRow) -> [(range: Range<String.Index>, id: String)] {
        artists(in: song.artist, credits: song.credits)
    }

    /// ranges in `text` (a display artist like "A & B"), in order.
    static func artists(in text: String, credits: [FeaturedArtist]) -> [(range: Range<String.Index>, id: String)] {
        var out: [(Range<String.Index>, String)] = []
        var from = text.startIndex
        for credit in credits {
            guard let id = credit.artistID, !credit.name.isEmpty,
                  let range = text.range(of: credit.name, range: from..<text.endIndex) else { continue }
            out.append((range, id))
            from = range.upperBound
        }
        return out
    }
}

struct LinkPiece: Identifiable {
    let id: Int
    let text: String
    let route: Route?

    static func artist(_ song: SongRow) -> [LinkPiece] { artists(song.artist, credits: song.credits) }

    /// An album's artist text, each credited album artist its own link. With no credits, the album's one artist id.
    static func artist(_ album: AlbumRow) -> [LinkPiece] {
        if album.credits.isEmpty, let id = album.artistID, !album.artist.isEmpty { return [LinkPiece(id: 0, text: album.artist, route: .artist(id))] }
        return artists(album.artist, credits: album.credits)
    }

    static func artists(_ text: String, credits: [FeaturedArtist]) -> [LinkPiece] {
        var pieces: [LinkPiece] = []
        var at = text.startIndex
        for link in SongLinks.artists(in: text, credits: credits) {
            if at < link.range.lowerBound { pieces.append(LinkPiece(id: pieces.count, text: String(text[at..<link.range.lowerBound]), route: nil)) }
            pieces.append(LinkPiece(id: pieces.count, text: String(text[link.range]), route: .artist(link.id)))
            at = link.range.upperBound
        }
        if at < text.endIndex { pieces.append(LinkPiece(id: pieces.count, text: String(text[at...]), route: nil)) }
        return pieces
    }

    /// keeps ids unique.
    static func joined(_ groups: [[LinkPiece]], separator: String) -> [LinkPiece] {
        var out: [LinkPiece] = []
        for group in groups where !group.isEmpty {
            if !out.isEmpty { out.append(LinkPiece(id: out.count, text: separator, route: nil)) }
            for piece in group { out.append(LinkPiece(id: out.count, text: piece.text, route: piece.route)) }
        }
        return out
    }

    static func plain(_ text: String) -> [LinkPiece] { text.isEmpty ? [] : [LinkPiece(id: 0, text: text, route: nil)] }

    static func title(_ song: SongRow) -> LinkPiece {
        LinkPiece(id: 0, text: song.title, route: song.albumID.isEmpty ? nil : .album(song.albumID))
    }

    static func album(_ song: SongRow) -> LinkPiece {
        LinkPiece(id: 0, text: song.album, route: song.albumID.isEmpty ? nil : .album(song.albumID))
    }
}

/// Link pieces on one line. A click opens the link, but with Command or Shift it goes to `modifiedClick` to extend
/// the row selection. Plain pieces let clicks through to the row.
struct LinkedText: View {
    let pieces: [LinkPiece]
    let shell: ShellModel
    var modifiedClick: (NSEvent.ModifierFlags) -> Void = { _ in }

    var body: some View {
        LinkRow {
            ForEach(pieces) { piece in
                if let route = piece.route {
                    LinkPieceView(text: piece.text) {
                        let flags = NSEvent.modifierFlags.intersection([.command, .shift, .control])
                        if flags.isEmpty { shell.go(route) } else { modifiedClick(flags) }
                    }
                } else {
                    Text(piece.text).lineLimit(1).truncationMode(.tail).allowsHitTesting(false)
                        .layoutValue(key: LinkRowSeparator.self, value: piece.text.trimmingCharacters(in: .whitespaces).count <= 1)
                }
            }
        }
        .fullTextHelp(pieces.map(\.text).joined())
        .clipped()
    }
}

/// Lays pieces out at their own widths like one line of text. The piece at the edge truncates and later ones get no
/// room. An HStack would squeeze every name.
nonisolated private struct LinkRow: Layout {
    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let sizes = subviews.map { $0.sizeThatFits(.unspecified) }
        let width = sizes.reduce(0) { $0 + $1.width }
        return CGSize(width: min(width, proposal.width ?? width), height: sizes.map(\.height).max() ?? 0)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        // A piece with room for only an ellipsis is dropped, along with the separator before it.
        var widths: [CGFloat] = []
        var x: CGFloat = 0
        for view in subviews {
            let ideal = view.sizeThatFits(.unspecified).width
            let room = bounds.width - x
            var width = min(ideal, max(0, room))
            if width < ideal && width < 24 { width = 0 }
            widths.append(width)
            x += width
        }
        for i in subviews.indices.dropFirst() where widths[i] == 0 && subviews[i - 1][LinkRowSeparator.self] { widths[i - 1] = 0 }
        x = bounds.minX
        for (view, width) in zip(subviews, widths) {
            guard width > 0 else {
                view.place(at: CGPoint(x: bounds.maxX, y: bounds.minY), proposal: .zero)
                continue
            }
            view.place(at: CGPoint(x: x, y: bounds.midY), anchor: .leading, proposal: ProposedViewSize(width: width, height: view.sizeThatFits(.unspecified).height))
            x += width
        }
    }
}

/// Marks a separator piece (" • ", ", "), so it goes when the name after it doesn't fit.
nonisolated private struct LinkRowSeparator: LayoutValueKey {
    static let defaultValue = false
}

private struct LinkPieceView: View {
    let text: String
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Text(text)
            .underline(hovering)
            .lineLimit(1)
            .truncationMode(.tail)
            // The text itself is the target, a few points taller than the line so it's easy to hit.
            .padding(.vertical, 3)
            .contentShape(Rectangle())
            .padding(.vertical, -3)
            .onHover { hovering = $0 }
            .pointerStyle(.link)
            .onTapGesture(perform: action)
            .accessibilityAddTraits(.isLink)
    }
}
