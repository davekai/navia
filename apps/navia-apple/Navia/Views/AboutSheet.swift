import SwiftUI

/// what the About sheet shows: an artist's facts and biography, or a page header's whole description.
enum AboutSubject {
    case artist(String)
    case text(AboutText)
}

/// the picture fading in at the top of the sheet.
enum AboutArt {
    case artwork(URL, ArtworkKind)
    case cover(PlaylistCover)

    @ViewBuilder var view: some View {
        switch self {
        case let .artwork(url, kind): ArtworkView(url: url, kind: kind, pixels: 1200, focusY: T.sheetCoverFocusY)
        case let .cover(cover): cover.asBand()
        }
    }
}

/// a header's description and what the sheet puts above it.
struct AboutText {
    let title: String
    var subtitle: String? = nil
    var art: AboutArt? = nil
    let text: String
}

/// the page's art fading into the sheet, its title and subtitle, then the parts given.
struct AboutSheet<Content: View>: View {
    let title: String
    var subtitle: String? = nil
    let art: AboutArt?
    let shell: ShellModel
    @ViewBuilder var content: Content

    var body: some View {
        DialogCoverSheet(art: art.map { AnyView($0.view) }, dismiss: { shell.closeAbout() }) {
            VStack(alignment: .leading, spacing: T.sheetSectionGap) {
                VStack(alignment: .leading, spacing: T.sheetLabelGap) {
                    Text(title)
                        .font(.navia(T.textTitle, .bold)).tracking(-0.026 * T.textTitle)
                        .foregroundStyle(T.tEm.color)
                        .shadow(T.heroTitleShadow)
                        .frame(minHeight: art == nil ? T.controlH : nil)
                        .padding(.leading, art == nil ? CoverSheet.headInset : 0)
                    if let subtitle {
                        Text(subtitle).font(.navia(T.textMD, .semibold)).foregroundStyle(T.t2.color)
                    }
                }
                content
            }
        }
    }
}

/// the sheet a header's MORE opens.
struct AboutTextLayer: View {
    let about: AboutText
    let shell: ShellModel

    var body: some View {
        AboutSheet(title: about.title, subtitle: about.subtitle, art: about.art, shell: shell) {
            AboutBody(text: about.text, ink: shell.theme.linkInk)
        }
    }
}

/// running text that keeps its line breaks; web addresses are links in `ink`, underlined under the pointer, and open
/// in the default browser.
struct AboutBody: View {
    let text: String
    let ink: RGBA
    @State private var hovered: Int?
    @State private var drawn = LinkRects()

    var body: some View {
        let links = WebLinks.find(in: text)
        styled(links)
            .font(.navia(T.textMD)).foregroundStyle(T.t1.color).lineSpacing(T.px(5))
            .fixedSize(horizontal: false, vertical: true)
            .textRenderer(LinkUnderlineRenderer(hovered: hovered, ink: ink.color, rects: drawn))
            .onContinuousHover { phase in
                switch phase {
                case let .active(point): hovered = drawn.link(at: point)
                case .ended: hovered = nil
                }
            }
            .pointerStyle(hovered == nil ? nil : .link)
            .onTapGesture { point in
                if let i = drawn.link(at: point), links.indices.contains(i), let url = URL(string: String(text[links[i]])) {
                    NSWorkspace.shared.open(url)
                }
            }
    }

    private func styled(_ links: [Range<String.Index>]) -> Text {
        var out = Text(verbatim: "")
        var at = text.startIndex
        for (i, range) in links.enumerated() {
            out = joined(out, Text(verbatim: String(text[at..<range.lowerBound])))
            out = joined(out, Text(verbatim: String(text[range])).foregroundStyle(ink.color).customAttribute(LinkRun(index: i)))
            at = range.upperBound
        }
        return joined(out, Text(verbatim: String(text[at...])))
    }

    private func joined(_ a: Text, _ b: Text) -> Text { Text("\(a)\(b)") }
}

/// marks the glyphs of the `index`th link for the renderer.
private struct LinkRun: TextAttribute {
    let index: Int
}

/// where each link was last drawn, in the text's own space, for hover and clicks.
final class LinkRects: @unchecked Sendable {
    fileprivate var rects: [(index: Int, rect: CGRect)] = []

    func link(at point: CGPoint) -> Int? {
        rects.first { $0.rect.contains(point) }?.index
    }
}

private struct LinkUnderlineRenderer: TextRenderer {
    let hovered: Int?
    let ink: Color
    let rects: LinkRects

    func draw(layout: Text.Layout, in context: inout GraphicsContext) {
        var found: [(index: Int, rect: CGRect)] = []
        for line in layout {
            for run in line {
                context.draw(run)
                guard let link = run[LinkRun.self] else { continue }
                let bounds = run.typographicBounds
                found.append((link.index, bounds.rect))
                if link.index == hovered {
                    let y = bounds.origin.y + T.linkUnderlineGap
                    context.fill(Path(CGRect(x: bounds.rect.minX, y: y, width: bounds.width, height: T.linkUnderline)), with: .color(ink))
                }
            }
        }
        rects.rects = found
    }
}

/// web addresses in running text; trailing punctuation usually ends the sentence, not the address.
enum WebLinks {
    private static let stop: Set<Character> = ["<", ">", "\""]
    private static let trailing: Set<Character> = [".", ",", ";", ":", "!", "?", ")", "]", "'", "\u{2019}", "\u{201d}", "\""]

    static func find(in text: String) -> [Range<String.Index>] {
        var links: [Range<String.Index>] = []
        var from = text.startIndex
        while let found = text.range(of: "http", range: from..<text.endIndex) {
            let rest = text[found.lowerBound...]
            guard let scheme = ["https://", "http://"].first(where: { rest.hasPrefix($0) }) else {
                from = found.upperBound
                continue
            }
            let end = rest.firstIndex { $0.isWhitespace || stop.contains($0) } ?? text.endIndex
            var last = end
            while last > found.lowerBound, trailing.contains(text[text.index(before: last)]) { last = text.index(before: last) }
            if text.distance(from: found.lowerBound, to: last) > scheme.count + 1 {
                links.append(found.lowerBound..<last)
            }
            from = max(end, text.index(found.lowerBound, offsetBy: scheme.count))
        }
        return links
    }
}

/// a page header's description: three lines at most, the last one's end fading before MORE, which shows only when
/// the text is cut and opens the whole text in the About sheet. the header never changes size.
struct HeaderDescription: View {
    let about: AboutText
    let shell: ShellModel
    @State private var fullHeight: CGFloat = 0
    @State private var shownHeight: CGFloat = 0
    @State private var moreWidth: CGFloat = 0

    private var clipped: Bool { fullHeight > shownHeight + 1 }

    var body: some View {
        Text(about.text)
            .font(.navia(T.textMD))
            .lineSpacing(T.px(4))
            .foregroundStyle(T.t2.color)
            .lineLimit(3)
            .fixedSize(horizontal: false, vertical: true)
            .background(alignment: .top) {
                Text(about.text).font(.navia(T.textMD)).lineSpacing(T.px(4))
                    .fixedSize(horizontal: false, vertical: true)
                    .hidden()
                    .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { fullHeight = $0 }
            }
            .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { shownHeight = $0 }
            .mask { if clipped { tailFade } else { Rectangle() } }
            .overlay(alignment: .bottomTrailing) {
                if clipped {
                    Button { shell.openAbout(about) } label: {
                        Text(L("MORE")).font(.navia(T.textMD, .semibold)).foregroundStyle(T.t1.color)
                            .frame(height: T.px(22)).contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .onGeometryChange(for: CGFloat.self, of: { $0.size.width }) { moreWidth = $0 }
                }
            }
            .frame(maxWidth: T.px(620), alignment: .leading)
    }

    /// opaque except the last line's end, which fades out over 40 pt and is clear under MORE.
    private var tailFade: some View {
        let line = shownHeight / 3
        return VStack(spacing: 0) {
            Rectangle()
            HStack(spacing: 0) {
                Rectangle()
                LinearGradient(colors: [.black, .clear], startPoint: .leading, endPoint: .trailing).frame(width: T.px(40))
                Color.clear.frame(width: moreWidth + T.px(8))
            }
            .frame(height: line)
        }
    }
}
