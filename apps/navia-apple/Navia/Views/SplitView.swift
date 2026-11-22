import SwiftUI

/// The split view, with the page's list on the left and the chosen item's page beside it. Each page remembers its choice.
enum PageLayout: String {
    case grid, split
}

struct LayoutToggle: View {
    @Binding var layout: String

    var body: some View {
        GlassGroup {
            IconButton(symbol: "list.bullet", label: L("Split view"), active: layout == PageLayout.split.rawValue, inGlass: true) { layout = PageLayout.split.rawValue }
            IconButton(symbol: "square.grid.2x2", label: L("Grid view"), active: layout == PageLayout.grid.rawValue, inGlass: true) { layout = PageLayout.grid.rawValue }
        }
    }
}

/// The list beside the chosen item's page. The list scrolls on its own, and the page gets its own page context.
/// Below `T.splitMinW` only the page shows.
struct SplitView<List: View, Detail: View>: View {
    var top: CGFloat = PagePadding.top
    @ViewBuilder let list: List
    @ViewBuilder let detail: Detail
    @Environment(\.pageContext) private var ctx

    var body: some View {
        let stacked = ctx.cssWidth <= T.splitMinW
        HStack(alignment: .top, spacing: 0) {
            if !stacked {
                ScrollView(.vertical) {
                    LazyVStack(spacing: 0) { list }
                        .padding(.trailing, T.splitListPad)
                }
                .scrollIndicators(.never)
                .overlay(alignment: .trailing) { Rectangle().fill(T.line.color).frame(width: 1) }
                .frame(width: T.splitListW)
                .padding(.top, top)
                .padding(.leading, ctx.sidebarUnder + T.gutter)
            }
            // The page keeps `ctx.end` at its right itself.
            detail
                .frame(maxWidth: .infinity)
                .environment(\.pageContext, beside(ctx, stacked: stacked))
        }
    }

    private func beside(_ page: PageContext, stacked: Bool) -> PageContext {
        var context = page
        context.gutter = T.splitGutter
        context.end = T.splitEnd
        let list = stacked ? T.splitEnd : T.gutter + T.splitListW + T.splitEnd
        if !stacked {
            context.sidebarUnder = 0
            context.shelfUnder = 0
        }
        context.pageWidth = max(0, page.pageWidth - list)
        context.layoutWidth = max(0, page.layoutWidth - list)
        return context
    }
}

/// One row of the split list, with a picture, a title and a quiet line. `on` while its page is showing.
struct SplitItem<Art: View>: View {
    let on: Bool
    let title: String
    let subtitle: String
    let action: () -> Void
    @ViewBuilder let art: Art

    var body: some View {
        Button(action: action) {
            HStack(spacing: T.splitRowGap) {
                art.frame(width: T.splitArt, height: T.splitArt)
                VStack(alignment: .leading, spacing: 0) {
                    Text(title).font(.navia(T.textMD, .medium)).lineLimit(1).fullTextHelp(title)
                    Text(subtitle).font(.navia(T.textMD)).foregroundStyle(T.t2.color).lineLimit(1).fullTextHelp(subtitle)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, T.splitRowPad)
            .frame(height: T.splitRowH)
            .background(alignment: .center) {
                RoundedRectangle(cornerRadius: T.rMD, style: .continuous)
                    .fill(on ? T.hover.color : .clear)
                    .padding(.vertical, T.splitRowInset)
            }
            .overlay(alignment: .bottomTrailing) { Rectangle().fill(T.line.color).frame(height: 1).padding(.leading, T.splitLineFrom) }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}
