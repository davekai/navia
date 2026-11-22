import AppKit
import NaviaCoreFFI
import SwiftUI

/// in order: the current cover (Edit Details only), the user's own picture, then the ready-made styles.
struct CoverSlots {
    let current: Bool
    var upload: Int { current ? 1 : 0 }
    var count: Int { upload + 1 + CoverPicker.styles.count }
    func style(at slot: Int) -> CoverStyle? {
        let index = slot - upload - 1
        return CoverPicker.styles.indices.contains(index) ? CoverPicker.styles[index] : nil
    }
    func isCurrent(_ slot: Int) -> Bool { current && slot == 0 }
}

/// the cover row of New Playlist and Edit Details; the styles are drawn by the core with `title`.
struct CoverPicker: View {
    static let styles = coverStyles()

    let title: String
    var current: PlaylistRow?
    @Binding var slot: Int
    @Binding var picked: URL?
    let accent: RGBA
    @State private var arts: [String: NSImage] = [:]
    @State private var scrolled: Int?
    @State private var hovering = false
    @FocusState private var focused: Bool
    @Environment(\.displayScale) private var scale

    private var slots: CoverSlots { CoverSlots(current: current != nil) }
    private var ring: CGFloat { T.coverPickRingW + T.coverPickRingGap }

    var body: some View {
        VStack(spacing: T.coverPickPagerTop) {
            stage
            pager
        }
        .task(id: title) { await draw() }
    }

    private var stage: some View {
        GeometryReader { geo in
            ScrollView(.horizontal) {
                HStack(spacing: T.coverPickGap) {
                    ForEach(0..<slots.count, id: \.self) { option($0).id($0) }
                }
                .scrollTargetLayout()
                .padding(.vertical, ring)
            }
            .contentMargins(.horizontal, max(0, (geo.size.width - T.coverPickSize) / 2), for: .scrollContent)
            .scrollTargetBehavior(.viewAligned)
            .scrollPosition(id: $scrolled, anchor: .center)
            .scrollIndicators(.never)
        }
        .frame(height: T.coverPickSize + 2 * ring)
        .overlay(alignment: .leading) { if slot > 0, showsArrows { arrow("chevron.left", L("Previous cover"), -1).padding(.leading, T.coverPickArrowInset) } }
        .overlay(alignment: .trailing) { if slot < slots.count - 1, showsArrows { arrow("chevron.right", L("Next cover"), 1).padding(.trailing, T.coverPickArrowInset) } }
        .onHover { hovering = $0 }
        // keyboard focus only: a click leaves the typing in the name field.
        .focusable(interactions: .activate)
        .focusEffectDisabled()
        .focused($focused)
        .onKeyPress(.leftArrow) { choose(slot - 1) }
        .onKeyPress(.upArrow) { choose(slot - 1) }
        .onKeyPress(.rightArrow) { choose(slot + 1) }
        .onKeyPress(.downArrow) { choose(slot + 1) }
        .onKeyPress(.home) { choose(0) }
        .onKeyPress(.end) { choose(slots.count - 1) }
        .onAppear { scrolled = slot }
        // A scroll by hand chooses the cover that comes to rest in the middle.
        .onChange(of: scrolled) { _, now in if let now, now != slot { slot = now } }
        .onChange(of: slot) { _, now in if scrolled != now { withAnimation(T.easeOut(T.shelfScroll)) { scrolled = now } } }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(L("Cover"))
    }

    private var showsArrows: Bool { hovering || focused || HoverDebug.forced("CoverArrows") }

    private func choose(_ to: Int) -> KeyPress.Result {
        slot = min(max(0, to), slots.count - 1)
        return .handled
    }

    private func arrow(_ symbol: String, _ label: String, _ step: Int) -> some View {
        RoundArrowButton(symbol: symbol, label: label, key: "CoverArrow") { slot = min(max(0, slot + step), slots.count - 1) }
    }

    @ViewBuilder private func option(_ index: Int) -> some View {
        let shape = RoundedRectangle(cornerRadius: T.rMD, style: .continuous)
        let chosen = index == slot
        Button { index == slots.upload ? pickUpload() : (slot = index) } label: {
            Group {
                if slots.isCurrent(index), let current {
                    PlaylistCover(playlist: current)
                } else if index == slots.upload {
                    uploadTile(shape)
                } else {
                    styleArt(index)
                }
            }
            .frame(width: T.coverPickSize, height: T.coverPickSize)
            .clipShape(shape)
            .contentShape(shape)
        }
        .buttonStyle(.plain)
        .overlay(
            RoundedRectangle(cornerRadius: T.rMD + ring, style: .continuous)
                .strokeBorder((focused ? accent : T.t1).color, lineWidth: T.coverPickRingW)
                .padding(-ring)
                .opacity(chosen ? 1 : 0)
                .animation(T.toggle, value: chosen)
                .allowsHitTesting(false))
        .help(label(index))
        .accessibilityLabel(label(index))
        .accessibilityAddTraits(chosen ? .isSelected : [])
    }

    private func label(_ index: Int) -> String {
        if slots.isCurrent(index) { return L("Current Cover") }
        return slots.style(at: index).map { L($0.name) } ?? L("Upload Image")
    }

    @ViewBuilder private func styleArt(_ index: Int) -> some View {
        if let style = slots.style(at: index), let art = arts[style.id] {
            Image(nsImage: art).resizable().interpolation(.high)
        } else {
            T.glass.color
        }
    }

    @ViewBuilder private func uploadTile(_ shape: RoundedRectangle) -> some View {
        if let picked, let image = NSImage(contentsOf: picked) {
            Image(nsImage: image).resizable().scaledToFill()
        } else {
            VStack(spacing: T.coverPickUploadGap) {
                Image(systemName: "photo").font(.system(size: T.coverPickUploadIcon))
                Text(L("Upload Image")).font(.navia(T.textSM))
            }
            .foregroundStyle(T.t2.color)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(T.glass.color)
            .overlay(shape.strokeBorder(T.glassEdge.color, lineWidth: 1))
        }
    }

    private var pager: some View {
        HStack(spacing: T.coverPickDotGap) {
            ForEach(0..<slots.count, id: \.self) { index in
                Button { index == slots.upload ? pickUpload() : (slot = index) } label: {
                    Group {
                        if index == slots.upload {
                            Image(systemName: "photo.fill").font(.system(size: T.coverPickPagerIcon))
                        } else {
                            Circle().frame(width: T.coverPickDot, height: T.coverPickDot)
                        }
                    }
                    .foregroundStyle((index == slot ? T.t1 : T.t3).color)
                    .animation(T.toggle, value: slot)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
        .frame(maxWidth: .infinity)
        .accessibilityHidden(true)
    }

    private func pickUpload() {
        if slot == slots.upload || picked == nil, let url = choosePlaylistImage() { picked = url }
        slot = slots.upload
    }

    /// off the main thread, the chosen one first.
    private func draw() async {
        let shown = title, side = UInt32((T.coverPickSize * min(scale, 2)).rounded())
        let first = slots.upload + 1
        let order = Self.styles.indices.sorted { abs($0 + first - slot) < abs($1 + first - slot) }
        for index in order {
            let id = Self.styles[index].id
            let png = await Task.detached { renderCoverPng(style: id, title: shown, size: side) }.value
            if Task.isCancelled { return }
            if let png, let image = NSImage(data: png) { arts[id] = image }
        }
    }
}

/// the core's PNG at its export size, in a temporary file.
func writeCoverImage(style: CoverStyle, title: String) async -> URL? {
    let id = style.id
    guard let png = await Task.detached(operation: { renderCoverPng(style: id, title: title, size: coverExportPx()) }).value else { return nil }
    let file = FileManager.default.temporaryDirectory.appendingPathComponent("navia-cover-\(UUID().uuidString).png")
    do { try png.write(to: file) } catch { return nil }
    return file
}

/// nothing for the current cover or an empty upload slot; returns what failed.
@MainActor func uploadChosenCover(slot: Int, slots: CoverSlots, picked: URL?, title: String, playlist id: String, library: any LibraryViewModel) async -> String? {
    if slot == slots.upload {
        guard let picked else { return nil }
        return await library.setPlaylistImage(id: id, path: picked.path)
    }
    guard let style = slots.style(at: slot), let file = await writeCoverImage(style: style, title: title) else { return nil }
    defer { try? FileManager.default.removeItem(at: file) }
    return await library.setPlaylistImage(id: id, path: file.path)
}
