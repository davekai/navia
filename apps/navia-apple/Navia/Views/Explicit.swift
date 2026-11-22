import SwiftUI

/// The small "E" after a title the server flags explicit, sized from the text beside it.
struct ExplicitMark: View {
    /// The font size of the title it follows.
    let size: CGFloat

    var body: some View {
        Text(verbatim: "E")
            .font(.system(size: size * 0.68, weight: .bold))
            .foregroundStyle(T.explicitInk.color)
            .frame(width: size * 0.85, height: size * 0.85)
            .background(RoundedRectangle(cornerRadius: 3, style: .continuous).fill(T.explicitFill.color))
            .fixedSize()
            .help(L("Explicit"))
            .accessibilityLabel(L("Explicit"))
    }
}

/// title and explicit mark on one line, so only the title truncates.
struct TitledText: View {
    let title: String
    let explicit: Bool
    let size: CGFloat
    var weight: Font.Weight = .regular

    var body: some View {
        HStack(spacing: size * 0.27) {
            Text(title).font(.navia(size, weight)).lineLimit(1).truncationMode(.tail).fullTextHelp(title)
            if explicit { ExplicitMark(size: size) }
        }
    }
}
