import NaviaCoreFFI
import SwiftUI

/// Listen-along views. The state lives in `ListenAlongModel`.

/// Whether another person's player is playing or paused. Shows nothing when the server doesn't report it.
struct LiveMark: View {
    let state: NowPlayingState?
    let accent: RGBA
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        switch state {
        case .paused:
            Image(systemName: "pause.fill").font(.system(size: T.liveMarkH * 0.8)).foregroundStyle(T.t3.color)
                .frame(height: T.liveMarkH).accessibilityLabel(L("Paused")).help(L("Paused"))
        case .playing, .starting:
            // Each bar runs at its own point of the cycle.
            TimelineView(.animation(paused: isSnapshot)) { context in
                let t = context.date.timeIntervalSinceReferenceDate
                HStack(alignment: .bottom, spacing: T.liveBarGap) {
                    ForEach(Array([0.4, 0.9, 0.2].enumerated()), id: \.offset) { _, offset in
                        Capsule().fill(accent.color)
                            .frame(width: T.liveBarW, height: T.liveMarkH * Self.height(t, offset))
                    }
                }
                .frame(height: T.liveMarkH, alignment: .bottom)
            }
            .accessibilityLabel(L("Playing")).help(L("Playing"))
        case nil:
            EmptyView()
        }
    }

    /// Rises and falls every 1.1 s, eased, between 30% and full height.
    private static func height(_ seconds: Double, _ offset: Double) -> CGFloat {
        let phase = (seconds / 1.1 + offset).truncatingRemainder(dividingBy: 1)
        let wave = 1 - abs(2 * phase - 1)
        let eased = wave * wave * (3 - 2 * wave)
        return 0.3 + 0.7 * eased
    }
}

/// One person in Listening Now. The row opens the album. The headphones button asks before following, and stops
/// following when on.
struct PersonRow: View {
    let entry: NowPlayingRow
    let shell: ShellModel
    @State private var hovering = false

    var body: some View {
        let song = SongRow(core: entry.song)
        let on = shell.player.followingFriend == nil && shell.player.followingUser == entry.user
        HStack(spacing: T.px(6)) {
            Button { if !song.albumID.isEmpty { shell.go(.album(song.albumID)) } } label: {
                HStack(spacing: T.px(12)) {
                    ArtworkView(url: song.artworkURL, pixels: Int(T.personArt * 2))
                        .frame(width: T.personArt, height: T.personArt)
                        .clipShape(RoundedRectangle(cornerRadius: T.rSM, style: .continuous))
                    VStack(alignment: .leading, spacing: 0) {
                        HStack(spacing: T.px(6)) {
                            Text(entry.user).font(.navia(T.textMD, .medium)).foregroundStyle(T.tNav.color).lineLimit(1)
                            LiveMark(state: entry.state, accent: shell.theme.accent)
                        }
                        Text("\(song.title) – \(song.artist)").font(.navia(T.textXS)).foregroundStyle(T.tNav.color).lineLimit(1).truncationMode(.tail)
                    }
                    Spacer(minLength: 0)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(L("{user}: {song} by {artist}", ["user": entry.user, "song": song.title, "artist": song.artist]))
            if entry.canListenAlong {
                IconButton(symbol: "headphones", label: on ? L("Stop listening along") : L("Listen along with {user}", ["user": entry.user]), active: on) {
                    if on { shell.player.stopFollowing() } else { shell.askListenAlong(entry.user) }
                }
                .opacity(on || hovering ? 1 : 0)
            }
        }
        .padding(.leading, T.px(14))
        .padding(.trailing, T.px(6))
        .frame(height: T.px(45))
        .background(RoundedRectangle(cornerRadius: T.rMD, style: .continuous).fill(hovering ? T.row.color : .clear))
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
    }
}

/// The confirm before following someone. It explains that the listener's own queue waits until following ends.
struct ListenAlongLayer: View {
    let user: String
    let shell: ShellModel

    var body: some View {
        if let listen = shell.player.listen, listen.listensAlongWithMe(user) {
            // following them back would make two people follow each other
            DialogLayer(title: L("{user} is listening along with you", ["user": user]), dismiss: { shell.closeListenAlong() }) {
                DialogText(text: L("{user} already hears what you play, so you’re listening together.", ["user": user]))
                DialogActions {
                    ActionButton(title: L("Done"), kind: .prominent(tint: shell.theme.accent)) { shell.closeListenAlong() }
                }
            }
        } else if let listen = shell.player.listen, let entry = listen.listing.first(where: { $0.user == user }) {
            let song = SongRow(core: entry.song)
            let vars = ["user": user, "song": song.title, "artist": song.artist]
            let own = shell.player.followingUser == nil && shell.player.station == nil ? shell.player.current?.title : nil
            DialogLayer(title: L("Listen along with {user}?", ["user": user]), dismiss: { shell.closeListenAlong() }) {
                DialogText(text: entry.state == .paused
                    ? L("{user} has paused “{song}” by {artist}. You’ll hear what {user} plays, from the same moment, and follow along as they skip, pause or seek.", vars)
                    : L("{user} is playing “{song}” by {artist}. You’ll hear what {user} plays, from the same moment, and follow along as they skip, pause or seek.", vars))
                DialogText(text: (own.map { L("Your queue waits. When you stop, it carries on from “{song}”.", ["song": $0]) } ?? L("Your queue isn’t changed.")) + " "
                    + L("Playing, pausing or skipping yourself stops listening along."))
                DialogNote(text: L("You share what you play too, so others using Navia on this server can see it and listen along."))
                DialogActions {
                    ActionButton(title: L("Cancel")) { shell.closeListenAlong() }
                    ActionButton(title: L("Listen Along"), kind: .prominent(tint: shell.theme.accent)) {
                        shell.closeListenAlong()
                        listen.listenAlong(user: user)
                    }
                }
            }
        }
    }
}

/// A Spotify friend's Discord avatar, or their initials, with the friend mark on its corner.
struct FriendAvatar: View {
    let friend: SpotifyFriendRow
    let theme: Theme

    var body: some View {
        ZStack {
            Circle().fill(T.panel2.color)
            Text(friend.initials).font(.navia(T.friendInitials, .semibold)).foregroundStyle(T.t2.color)
            if let url = friend.avatarUrl.flatMap(URL.init(string:)) {
                AvatarImage(url: url, pixels: Int(T.personArt * 2))
            }
        }
        .frame(width: T.personArt, height: T.personArt)
        .clipShape(Circle())
        .overlay(alignment: .bottomTrailing) {
            ZStack {
                Circle().fill(theme.accent.color)
                Image(systemName: "link").font(.system(size: T.friendMarkGlyph, weight: .semibold)).foregroundStyle(theme.onAccent.color)
            }
            .frame(width: T.friendMark, height: T.friendMark)
            .offset(x: T.px(2), y: T.px(2))
        }
    }
}

/// An avatar from a URL, shown once it loads. The initials stay underneath until then, or if it fails.
private struct AvatarImage: View {
    let url: URL
    let pixels: Int
    @State private var image: NSImage?

    var body: some View {
        // A view that exists before the image, since `.task` never runs on an empty `Group`.
        Color.clear
            .overlay {
                if let image { Image(nsImage: image).resizable().interpolation(.high).aspectRatio(contentMode: .fill) }
            }
            .task(id: url) { image = await ArtworkLoader.shared.load(url, pixels: pixels) }
    }
}

/// One Spotify friend in Listening Now. "Not in your library" shows when their song isn't in it. The row opens the
/// album when it is, and the headphones button asks before following and stops it when on.
struct FriendRow: View {
    let friend: SpotifyFriendRow
    let shell: ShellModel
    @State private var hovering = false

    var body: some View {
        let on = shell.player.followingFriend == friend.id
        let song = friend.song.map { SongRow(core: $0) }
        let line = "\(friend.title) – \(friend.artist)"
        HStack(spacing: T.px(6)) {
            Button { if let album = song?.albumID, !album.isEmpty { shell.go(.album(album)) } } label: {
                HStack(spacing: T.px(12)) {
                    FriendAvatar(friend: friend, theme: shell.theme)
                    VStack(alignment: .leading, spacing: 0) {
                        HStack(spacing: T.px(6)) {
                            Text(friend.name).font(.navia(T.textMD, .medium)).foregroundStyle(T.tNav.color).lineLimit(1)
                            if song != nil { LiveMark(state: .playing, accent: shell.theme.accent) }
                        }
                        Text(song != nil ? line : "\(L("Not in your library")) · \(line)")
                            .font(.navia(T.textXS)).foregroundStyle(T.tNav.color).lineLimit(1).truncationMode(.tail)
                    }
                    Spacer(minLength: 0)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help(song != nil
                  ? L("{user} on Spotify: {song} by {artist}", ["user": friend.name, "song": friend.title, "artist": friend.artist])
                  : L("{user} on Spotify: {song} by {artist}. Not in your library.", ["user": friend.name, "song": friend.title, "artist": friend.artist]))
            IconButton(symbol: "headphones", label: on ? L("Stop listening along") : L("Listen along with {user}", ["user": friend.name]), active: on) {
                if on { shell.player.stopFollowing() } else { shell.askListenAlongFriend(friend.id) }
            }
            .opacity(on || hovering ? 1 : 0)
        }
        .padding(.leading, T.px(14))
        .padding(.trailing, T.px(6))
        .frame(height: T.px(45))
        .background(RoundedRectangle(cornerRadius: T.rMD, style: .continuous).fill(hovering ? T.row.color : .clear))
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
    }
}

/// The confirm before following a Spotify friend. It explains that songs play from the listener's own library and
/// that the queue waits.
struct ListenAlongFriendLayer: View {
    let id: String
    let shell: ShellModel

    var body: some View {
        if let listen = shell.player.listen, let friend = listen.friends.first(where: { $0.id == id }), friend.listening {
            let vars = ["user": friend.name, "song": friend.title, "artist": friend.artist]
            let own = shell.player.followingUser == nil && shell.player.station == nil ? shell.player.current?.title : nil
            DialogLayer(title: L("Listen along with {user}?", ["user": friend.name]), dismiss: { shell.closeListenAlong() }) {
                DialogText(text: friend.song != nil
                    ? L("{user} is playing “{song}” by {artist} on Spotify. You’ll hear it from your library, from the same moment, and follow along as they change songs.", vars)
                    : L("{user} is playing “{song}” by {artist} on Spotify, which isn’t in your library. You’ll hear their next song that is, from the same moment.", vars))
                DialogText(text: (own.map { L("Your queue waits. When you stop, it carries on from “{song}”.", ["song": $0]) } ?? L("Your queue isn’t changed.")) + " "
                    + L("Playing, pausing or skipping yourself stops listening along."))
                DialogNote(text: L("Songs that aren’t in your library are skipped. Navia reads only what your friend shares through Lanyard, and nothing you play is shared with them."))
                DialogActions {
                    ActionButton(title: L("Cancel")) { shell.closeListenAlong() }
                    ActionButton(title: friend.song != nil ? L("Listen Along") : L("Listen Along Anyway"), kind: .prominent(tint: shell.theme.accent)) {
                        shell.closeListenAlong()
                        listen.listenAlongFriend(id: id)
                    }
                }
            }
        }
    }
}
