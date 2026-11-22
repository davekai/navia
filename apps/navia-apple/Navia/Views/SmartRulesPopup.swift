import AppKit
import SwiftUI

/// The smart playlist mark on the playlist header. With the rules known it shows their count and opens them.
/// The rules come from the library, never a fresh `getPlaylist`, which would re-evaluate the playlist.
struct SmartRulesPill: View {
    let shell: ShellModel
    let playlist: PlaylistRow
    @State private var probe = WindowFrameProbe()
    @State private var hovering = false
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        if let rules = playlist.rules, !rules.rules.isEmpty {
            let open = shell.popups.rules?.playlistID == playlist.id
            Button {
                if open { shell.popups.close(); return }
                let id = playlist.id
                shell.popups.open(.init(playlistID: id, summary: rules, anchor: probe.frame(), onEdit: { [shell] in shell.go(.smartEditor(id)) }, copy: !playlist.isOwned))
            } label: {
                content(count: rules.rules.count)
            }
            .buttonStyle(.plain)
            .glassPill()
            .overlay(HoverFillBackground(on: hovering || open || HoverDebug.forced("Smart")))
            .background { if !isSnapshot { WindowFrameReader(probe: probe) } }
            .onHover { hovering = $0 }
            #if DEBUG
            // `-naviaSmartRulesOpen YES` opens the rules once the page is up, for captures without real input.
            .task {
                guard UserDefaults.standard.bool(forKey: "naviaSmartRulesOpen") else { return }
                try? await Task.sleep(for: .seconds(2))
                let id = playlist.id
                shell.popups.open(.init(playlistID: id, summary: rules, anchor: probe.frame(), onEdit: { [shell] in shell.go(.smartEditor(id)) }, copy: !playlist.isOwned))
            }
            #endif
            .accessibilityLabel("\(L("Smart")), \(Ln("{count} rule", "{count} rules", rules.rules.count))")
            .accessibilityHint(L("Smart playlist rules"))
        } else {
            label.padding(.horizontal, T.px(10)).frame(height: T.px(26)).glassPill()
        }
    }

    private var label: some View {
        HStack(spacing: T.px(5)) {
            Image(systemName: "bolt.fill").font(.system(size: T.px(11))).foregroundStyle(shell.theme.accent.color)
            Text(L("Smart")).font(.navia(T.textSM, .medium)).foregroundStyle(T.t1.color)
        }
    }

    private func content(count: Int) -> some View {
        HStack(spacing: T.px(5)) {
            label
            Text("· " + Ln("{count} rule", "{count} rules", count)).font(.navia(T.textSM)).foregroundStyle(T.t2.color)
            Image(systemName: "chevron.down").font(.system(size: T.px(9), weight: .bold)).foregroundStyle(T.t2.color)
        }
        .lineLimit(1)
        .fixedSize()
        .padding(.leading, T.px(9))
        .padding(.trailing, T.px(10))
        .frame(height: T.px(26))
        .contentShape(Capsule())
    }
}

/// The pill's frame in window coordinates from the top left. The header is hosted inside the songs table, where
/// SwiftUI's `.global` is the host view's space, not the window's.
final class WindowFrameProbe {
    weak var view: NSView?

    func frame() -> CGRect {
        guard let view, let content = view.window?.contentView else { return .zero }
        let r = view.convert(view.bounds, to: nil)
        return CGRect(x: r.minX, y: content.bounds.height - r.maxY, width: r.width, height: r.height)
    }
}

struct WindowFrameReader: NSViewRepresentable {
    let probe: WindowFrameProbe
    func makeNSView(context: Context) -> NSView {
        let v = NSView()
        probe.view = v
        return v
    }
    func updateNSView(_ nsView: NSView, context: Context) { probe.view = nsView }
}

/// The rules list, with the summary, match mode, each rule, then sort and limit, and an edit button. Scrolls past
/// a fixed height. Esc, a click outside or a resize closes it.
struct SmartRulesLayer: View {
    let request: PopupCenter.RulesRequest
    let center: PopupCenter
    let windowSize: CGSize

    @State private var contentHeight: CGFloat = 0
    @State private var position = ScrollPosition(edge: .top)
    @State private var offset: CGFloat = 0
    @State private var viewport: CGFloat = 0
    @FocusState private var focused: Bool
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        let footer = request.onEdit == nil ? 0 : T.px(49)
        return AnchoredPopover(anchor: request.anchor, width: T.popW, wanted: contentHeight == 0 ? 0 : contentHeight + footer, windowSize: windowSize, close: { center.close() }) {
            panel
        }
        .onAppear { focused = true }
        #if DEBUG
        .task { await debugKeys() }
        #endif
    }

    #if DEBUG
    /// `-naviaSmartRulesKeys down,end,escape` sends those keys into this window, one every 1.5 s.
    private func debugKeys() async {
        guard let list = UserDefaults.standard.string(forKey: "naviaSmartRulesKeys") else { return }
        let codes: [String: (UInt16, String)] = ["down": (125, "\u{F701}"), "up": (126, "\u{F700}"), "pagedown": (121, "\u{F72D}"), "end": (119, "\u{F72B}"), "home": (115, "\u{F729}"), "escape": (53, "\u{1B}")]
        for name in list.split(separator: ",").map(String.init) {
            try? await Task.sleep(for: .seconds(1.5))
            guard let (code, chars) = codes[name], let window = NSApp.windows.first(where: { $0.isVisible && $0.contentView != nil }) else { continue }
            for type in [NSEvent.EventType.keyDown, .keyUp] {
                if let e = NSEvent.keyEvent(with: type, location: .zero, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil, characters: chars, charactersIgnoringModifiers: chars, isARepeat: false, keyCode: code) {
                    window.sendEvent(e)
                }
            }
            try? await Task.sleep(for: .milliseconds(400))
            FileHandle.standardError.write(Data("smartRulesKey \(name) open=\(center.rules != nil) offset=\(Int(offset)) viewport=\(Int(viewport)) content=\(Int(contentHeight)) focused=\(focused)\n".utf8))
        }
    }
    #endif

    private var panel: some View {
        VStack(spacing: 0) {
            if isSnapshot {
                // ImageRenderer can't draw a ScrollView, so checks see the list's top instead.
                Color.clear.overlay(alignment: .top) { list }.clipped()
            } else {
            ScrollView(.vertical) {
                list.onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { contentHeight = $0 }
            }
            .scrollPosition($position)
            .onScrollGeometryChange(for: CGSize.self, of: { CGSize(width: $0.contentOffset.y, height: $0.containerSize.height) }) { _, g in offset = g.width; viewport = g.height }
            .scrollIndicators(.automatic)
            .focusable()
            .focusEffectDisabled()
            .focused($focused)
            .onKeyPress(.escape) { center.close(); return .handled }
            .onKeyPress(.downArrow) { scroll(by: T.px(40)) }
            .onKeyPress(.upArrow) { scroll(by: -T.px(40)) }
            .onKeyPress(.pageDown) { scroll(by: viewport - T.px(40)) }
            .onKeyPress(.pageUp) { scroll(by: -(viewport - T.px(40))) }
            .onKeyPress(.home) { position.scrollTo(edge: .top); return .handled }
            .onKeyPress(.end) { position.scrollTo(edge: .bottom); return .handled }
            .accessibilityElement(children: .contain)
            .accessibilityLabel(L("Smart playlist rules"))
            }
            if let onEdit = request.onEdit {
                Rectangle().fill(T.line.color).frame(height: 1)
                Button {
                    center.close()
                    onEdit()
                } label: {
                    HStack(spacing: T.px(11)) {
                        Image(systemName: request.copy ? "doc.on.doc" : "pencil").font(.system(size: T.px(14)))
                        Text(request.copy ? L("Edit a Copy") : L("Edit Rules")).font(.navia(T.textMD))
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, T.px(11))
                    .frame(height: T.px(36))
                    .contentShape(Rectangle())
                }
                .buttonStyle(MenuRowStyle())
                .padding(T.px(6))
                .onKeyPress(.escape) { center.close(); return .handled }
            }
        }
        .accessibilityAddTraits(.isModal)
    }

    private var list: some View {
        RuleListView(summary: request.summary)
            .padding(.horizontal, T.px(16))
            .padding(.top, T.px(14))
            .padding(.bottom, T.px(12))
    }

    private func scroll(by delta: CGFloat) -> KeyPress.Result {
        position.scrollTo(y: max(0, offset + delta))
        return .handled
    }
}

private struct MenuRowStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View { HoverRow(label: configuration.label, pressed: configuration.isPressed) }
}

private struct HoverRow<Label: View>: View {
    let label: Label
    let pressed: Bool
    @State private var hovering = false
    var body: some View {
        label
            .foregroundStyle(T.t1.color)
            .background(RoundedRectangle(cornerRadius: T.px(10), style: .continuous).fill(hovering || pressed ? T.hover.color : .clear))
            .onHover { hovering = $0 }
    }
}

/// A smart playlist's rules, with a summary line, the match mode, one rule a line, then the order and limit.
/// Shown by the rules popover and Get Info.
struct RuleListView: View {
    let summary: RuleSummary

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if !summary.sentence.isEmpty {
                Text(summary.sentence).font(.navia(T.textMD)).foregroundStyle(T.t1.color).lineSpacing(T.textMD * 0.35)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.bottom, T.px(10))
                Rectangle().fill(T.line.color).frame(height: 1).padding(.bottom, T.px(10))
            }
            if !summary.match.isEmpty {
                Text(summary.match).font(.navia(T.textXS)).foregroundStyle(T.t2.color).padding(.bottom, T.px(4))
            }
            ForEach(Array(summary.rules.enumerated()), id: \.offset) { index, rule in
                VStack(alignment: .leading, spacing: 0) {
                    if index > 0 { Rectangle().fill(T.line.color).frame(height: 1) }
                    Text(rule).font(.navia(T.textMD)).foregroundStyle(T.t1.color)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.vertical, T.px(8))
                }
            }
            if !summary.order.isEmpty {
                Rectangle().fill(T.line.color).frame(height: 1).padding(.top, T.px(6))
                Text(summary.order.joined(separator: " · ")).font(.navia(T.textXS)).foregroundStyle(T.t2.color)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.top, T.px(10))
            }
        }
    }
}
