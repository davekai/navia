import SwiftUI

/// The player's chip slot, one quiet pill above the title or above the bar's play controls. It shows, in order of
/// priority, a lost AirPlay receiver, "Listening along with <user>" while following, a brief note when following
/// ends by itself, and the launch offer of another device's newer queue.
struct PlayerChipSlot: View {
    let player: any PlayerViewModel
    /// The bar's version, with the bar's glass and a float shadow.
    var overBar = false

    static func shows(_ player: any PlayerViewModel) -> Bool {
        AirPlayModel.shared.lost || player.followingUser != nil || player.listen?.ended != nil || hostListeners(player) != nil || player.queueOfferDevice != nil
    }

    /// For a host, "<user> is listening along", while listeners are allowed to control playback.
    static func hostListeners(_ player: any PlayerViewModel) -> String? {
        guard let listen = player.listen, listen.controlOn, player.followingUser == nil, let first = listen.listeners.first else { return nil }
        return listen.listeners.count == 1 ? L("{user} is listening along", ["user": first.user])
            : Ln("{count} person is listening along", "{count} people are listening along", listen.listeners.count)
    }

    var body: some View {
        let airplay = AirPlayModel.shared
        if airplay.lost, let receiver = airplay.active {
            let gaveUp = airplay.lostGaveUp
            PlayerChip(symbol: AirPlayGlyph.symbol, label: gaveUp ? L("Couldn’t reconnect") : L("Connection lost. Reconnecting…"), overBar: overBar,
                       help: receiver.name,
                       end: gaveUp ? .two(L("Try Again"), { airplay.retry() }, L("Play Here")) : .text(L("Play Here")), onEnd: { airplay.playHere() })
        } else if let user = player.followingUser {
            let timing = player.listen?.timing
            let label = player.followingWaiting ? L("Waiting for a song you have from {user}", ["user": user])
                : player.followingSending ? L("Sending to {user}…", ["user": user])
                : player.followingHeld ? L("Listening along with {user} · paused", ["user": user])
                : L("Listening along with {user}", ["user": user])
            PlayerChip(symbol: "headphones", label: label, overBar: overBar,
                       help: timing.map { L("Checks every {seconds} seconds and catches up when it drifts more than {drift} seconds.", ["seconds": "\($0.followPollMs / 1000)", "drift": "\($0.resyncDriftMs / 1000)"]) },
                       end: .text(L("Stop")), onEnd: { player.stopFollowing() })
        } else if let user = player.listen?.ended {
            PlayerChip(symbol: "headphones", label: L("{user} stopped listening", ["user": user]), overBar: overBar,
                       end: .close(L("Dismiss")), onEnd: { player.listen?.dismissEnded() })
        } else if let label = Self.hostListeners(player) {
            PlayerChip(symbol: "headphones", label: label, overBar: overBar, end: .none, onEnd: {})
        } else if let device = player.queueOfferDevice {
            PlayerChip(symbol: "iphone", label: L("Continue from {device}?", ["device": device]), overBar: overBar,
                       onLabel: { player.answerQueueOffer(accept: true) }, end: .close(L("Not now")), onEnd: { player.answerQueueOffer(accept: false) })
        }
    }
}

/// One chip, with a symbol and a label, then its end control. The label is a button when `onLabel` is given.
private struct PlayerChip: View {
    /// `two` is a first word with its own action, then a second that runs `onEnd`.
    enum End { case text(String), two(String, () -> Void, String), close(String), none }
    let symbol: String
    let label: String
    var overBar = false
    var help: String? = nil
    var onLabel: (() -> Void)? = nil
    let end: End
    let onEnd: () -> Void
    @Environment(\.playerDim) private var dim

    var body: some View {
        HStack(spacing: T.px(2)) {
            let content = HStack(spacing: T.px(6)) {
                Image(systemName: symbol).font(.system(size: T.px(12)))
                Text(label).lineLimit(1).truncationMode(.tail)
            }
            .padding(.vertical, T.px(4))
            .padding(.trailing, T.px(4))
            .contentShape(Rectangle())
            if let onLabel {
                Button(action: onLabel) { content }.buttonStyle(HoverBrightenStyle(key: "chip-\(symbol)"))
            } else {
                content.opacity(0.9)
            }
            switch end {
            case .text(let title):
                endWord(title, action: onEnd)
            case .two(let first, let firstAction, let second):
                endWord(first, action: firstAction)
                endWord(second, action: onEnd)
            case .close(let title):
                Button(action: onEnd) {
                    Image(systemName: "xmark").font(.system(size: T.px(10), weight: .bold))
                        .frame(width: T.px(24), height: T.px(24))
                        .contentShape(Rectangle())
                }
                .buttonStyle(HoverBrightenStyle(key: "chip-x-\(symbol)"))
                .accessibilityLabel(title)
                .help(title)
            case .none:
                // Keeps the same height as the others without a button.
                Color.clear.frame(width: T.px(6), height: T.px(24))
            }
        }
        .font(.navia(T.textSM))
        .padding(.leading, T.px(10))
        .padding(.trailing, T.px(2))
        .padding(.vertical, T.px(2))
        .foregroundStyle(overBar ? T.barInk.color : (dim ?? T.tOnArt.color))
        .modifier(OfferGlass(overBar: overBar))
        .help(help ?? "")
    }
}

private extension PlayerChip {
    /// A word at the chip's end, such as Stop. It brightens on hover like the other small controls.
    func endWord(_ title: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title).font(.navia(T.textSM, .medium)).padding(.horizontal, T.px(10)).frame(height: T.px(24)).contentShape(Capsule())
        }
        .buttonStyle(HoverBrightenStyle(key: "chip-end-\(symbol)-\(title)"))
    }
}

private struct OfferGlass: ViewModifier {
    let overBar: Bool
    func body(content: Content) -> some View {
        content
            .plainGlass(Capsule(), flat: overBar ? T.floatGlass : T.glassOn)
            .shadow(color: overBar ? .black.opacity(0.35) : .clear, radius: T.px(12), y: T.px(6))
    }
}
