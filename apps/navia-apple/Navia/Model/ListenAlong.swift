import Foundation
import NaviaCoreFFI
import Observation

/// Listening Now and following on the same server. The rules live in the core (`listeningNow`, `followStep`). This
/// keeps the list fresh: every 15 s while it shows, every 5 s while following, and when the followed song ends.
///
/// Spotify friends come from Lanyard through the core. They show in Private mode too, since following them reveals
/// nothing.
@MainActor @Observable final class ListenAlongModel {
    private(set) var listing: [NowPlayingRow] = []
    /// When `listing` was last read. It's dropped after `timing.listingKeepMs`, so failing checks never leave a stale
    /// list.
    @ObservationIgnored private var listingAt: CFAbsoluteTime?
    /// "<user> stopped listening", shown in the player for a few seconds after following ends on its own.
    private(set) var ended: String?

    @ObservationIgnored private let core: NaviaCore
    @ObservationIgnored private weak var player: CorePlayerModel?
    @ObservationIgnored private var nextCheck: CFAbsoluteTime = 0
    @ObservationIgnored private var checking = false
    @ObservationIgnored private var loop: Task<Void, Never>?
    @ObservationIgnored private var endedGeneration = 0
    @ObservationIgnored let timing = listenAlongTiming()

    /// Spotify friends as Lanyard last reported them, with songs matched in the library.
    private(set) var friends: [SpotifyFriendRow] = []
    /// Settings > Spotify Friends, and the Lanyard address (empty for the public one). Kept for every account.
    private(set) var savedFriends: [SpotifyFriend]
    var lanyardAddress: String {
        didSet { UserDefaults.standard.set(lanyardAddress, forKey: Self.addressKey) }
    }
    @ObservationIgnored private var friendsGeneration: UInt64?
    @ObservationIgnored private var friendsChecking = false
    private static let friendsKey = "spotifyFriends"
    private static let addressKey = "lanyardAddress"

    init(core: NaviaCore, player: CorePlayerModel) {
        self.core = core
        self.player = player
        let stored = UserDefaults.standard.array(forKey: Self.friendsKey) as? [[String: String]] ?? []
        savedFriends = stored.compactMap { d in d["id"].map { SpotifyFriend(id: $0, name: d["name"]) } }
        lanyardAddress = UserDefaults.standard.string(forKey: Self.addressKey) ?? ""
        listenersControl = UserDefaults.standard.bool(forKey: Self.controlKey)
        core.setSpotifyFriends(address: lanyardAddress, friends: savedFriends)
        loop = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(1))
                self?.tick()
            }
        }
    }

    deinit { loop?.cancel() }

    /// Others are listed only while this listener shares too: sharing on, Private mode off, connected.
    var active: Bool {
        guard let player else { return false }
        return player.shareListening && !player.privateMode && !player.library.isOffline
    }

    /// Sharing or Private mode changed. Off ends following on the server and empties the list, on checks at once.
    /// Following a Spotify friend carries on.
    func refreshActive() {
        if active {
            nextCheck = 0
        } else {
            if player?.followingUser != nil, player?.followingFriend == nil { player?.stopFollowing() }
            if !listing.isEmpty { listing = [] }
        }
    }

    private func tick() {
        friendsTick()
        applyMarks()
        guard active else {
            player?.sessionCheck()
            if !listing.isEmpty { listing = [] }
            return
        }
        if !listing.isEmpty, listingAt.map({ CFAbsoluteTimeGetCurrent() - $0 > Double(timing.listingKeepMs) / 1000 }) ?? true { listing = [] }
        if CFAbsoluteTimeGetCurrent() >= nextCheck { checkNow() }
    }

    /// Checks the server now, or the friends while following a Spotify friend.
    func checkNow() {
        if player?.followingFriend != nil { refreshFriends(); return }
        guard !checking, active else { return }
        checking = true
        Task { [weak self] in
            guard let self else { return }
            let entries = try? await core.nowPlaying()
            checking = false
            // while listeners may control playback here, often enough to notice them join and act; faster at a session song's end
            let session = listenCheckEveryMs(session: sessionOn, leftMs: player?.sessionLeftMs, waiting: player?.handover.waiting() ?? false)
            let every = player?.along != nil || controlOn ? session : timing.listPollMs
            nextCheck = CFAbsoluteTimeGetCurrent() + Double(every) / 1000
            guard let entries else { return }
            listing = core.listeningNow(entries: entries, active: active)
            listingAt = CFAbsoluteTimeGetCurrent()
            if let player, let along = player.along, player.followingFriend == nil, let local = player.followLocal() {
                let step = followStep(user: along.user, player: along.player, entries: entries, local: local)
                let seen = followSeen(user: along.user, player: along.player, entries: entries, nowMs: listenNowMs())
                let twoWay = followsBack(user: along.user, player: along.player, entries: entries, viewer: core.username())
                player.applyFollowStep(step)
                player.noteSeen(seen, twoWay: twoWay)
            }
            hostCheck(entries)
            player?.sessionCheck()
            applyMarks()
        }
    }

    /// Listen Along from the confirm: follows `user` as listed now.
    func listenAlong(user: String) {
        guard let entry = listing.first(where: { $0.user == user }) else { return }
        ended = nil
        player?.follow(entry)
        nextCheck = CFAbsoluteTimeGetCurrent() + Double(timing.followPollMs) / 1000
    }

    /// the player says so for a few seconds.
    func followingEnded(user: String) {
        ended = user
        endedGeneration &+= 1
        let generation = endedGeneration
        Task { [weak self, ms = timing.endedNoticeMs] in
            try? await Task.sleep(for: .milliseconds(ms))
            guard let self, endedGeneration == generation else { return }
            ended = nil
        }
    }

    func dismissEnded() { ended = nil }

    // MARK: Listeners controlling this Navia

    /// Settings > Let listeners control playback. Off by default.
    var listenersControl: Bool {
        didSet {
            UserDefaults.standard.set(listenersControl, forKey: Self.controlKey)
            applyMarks()
        }
    }
    private static let controlKey = "listenersControl"
    /// at the last look, while the setting applies.
    private(set) var listeners: [ListenerRow] = []
    @ObservationIgnored private var marks: (control: Bool, following: Bool, queued: Bool)?

    /// Sharing on, Private mode off, and the setting on.
    var controlOn: Bool { active && listenersControl }

    /// The reports' marks follow the setting and whether this Navia follows someone.
    private func applyMarks() {
        let control = controlOn
        let following = player.map { $0.along != nil && $0.followingFriend == nil } ?? false
        let queued = sessionOn && (player?.sessionMine ?? false)
        guard marks.map({ $0.control != control || $0.following != following || $0.queued != queued }) ?? true else { return }
        marks = (control, following, queued)
        core.setListenMarks(control: control, following: following, queued: queued)
    }

    /// The host side, when not following anyone: who listens along, with their changes mirrored here.
    private func hostCheck(_ entries: [NowPlayingRow]) {
        guard active, let player, let host = player.hostRow() else {
            if !listeners.isEmpty { listeners = [] }
            return
        }
        let now = core.listeners(entries: entries, host: host, prev: listeners)
        let mirror = hostMirror(host: host, prev: listeners, now: now, control: controlOn)
        listeners = now
        if let mirror { player.applyHostMirror(mirror) }
    }

    /// a listening session: following someone whose Navia lets listeners control it, or hosting listeners with it on.
    var sessionOn: Bool {
        guard active, let player else { return false }
        if let along = player.along { return player.followingFriend == nil && allowsControl(player: along.player) }
        return controlOn && !listeners.isEmpty
    }

    /// who on the other side has songs queued for the session (their titles can't be sent).
    var sessionTheirs: [String] {
        guard let player else { return [] }
        if let along = player.along { return hasSessionQueue(player: along.player) ? [along.user] : [] }
        return listeners.filter(\.queued).map(\.user)
    }

    /// following them too would make two people follow each other.
    func listensAlongWithMe(_ user: String) -> Bool {
        listeners.contains { $0.user.lowercased() == user.lowercased() }
    }

    // MARK: Spotify friends

    /// whenever there's a library to play from and the server is reachable, Private mode included.
    var friendsActive: Bool { player.map { !$0.library.isOffline } ?? false }

    /// those whose activity shows a song.
    var listeningFriends: [SpotifyFriendRow] { friendsActive ? friends.filter(\.listening) : [] }

    /// false when the id isn't one or is already added.
    @discardableResult func addFriend(id text: String, name: String) -> Bool {
        guard let id = lanyardFriendId(text: text), !savedFriends.contains(where: { $0.id == id }) else { return false }
        let trimmed = name.trimmingCharacters(in: .whitespaces)
        savedFriends.append(SpotifyFriend(id: id, name: trimmed.isEmpty ? nil : trimmed))
        saveFriends()
        return true
    }

    /// A friend followed now stops being followed.
    func removeFriend(id: String) {
        savedFriends.removeAll { $0.id == id }
        friends.removeAll { $0.id == id }
        if player?.followingFriend == id { player?.stopFollowing() }
        saveFriends()
    }

    /// applied once it's been entered.
    func applyAddress() {
        core.setSpotifyFriends(address: lanyardAddress, friends: savedFriends)
        friendsGeneration = nil
    }

    private func saveFriends() {
        UserDefaults.standard.set(savedFriends.map { f in f.name.map { ["id": f.id, "name": $0] } ?? ["id": f.id] }, forKey: Self.friendsKey)
        applyAddress()
    }

    /// Called when Lanyard has something new, and every second while following a friend.
    private func friendsTick() {
        let following = player?.followingFriend != nil
        core.setFollowingFriend(on: following)
        guard following || friendsGeneration != core.spotifyGeneration() else { return }
        refreshFriends()
    }

    /// Reads the friends off the main thread, since matching reads the library, and steps the follow.
    func refreshFriends() {
        guard !friendsChecking else { return }
        friendsChecking = true
        let core = core
        Task { [weak self] in
            // Read the generation first, so a change landing during the read is picked up next tick.
            let (generation, rows) = await Task.detached(priority: .utility) { (core.spotifyGeneration(), core.spotifyFriends()) }.value
            guard let self else { return }
            friendsChecking = false
            if rows != friends { friends = rows }
            friendsGeneration = generation
            guard let player, let along = player.along, player.followingFriend != nil else { return }
            guard let local = player.followLocal() else { return }
            player.applyFollowStep(core.spotifyFollowStep(user: along.user, player: along.player, local: local))
        }
    }

    /// Listen Along with a friend: their song from their position when it's in the library. Otherwise following starts
    /// paused on the listener's own song, and their next song in the library plays.
    func listenAlongFriend(id: String) {
        guard let friend = friends.first(where: { $0.id == id }), friend.listening, let player else { return }
        ended = nil
        if let entry = core.spotifySession(id: id) {
            player.follow(entry)
        } else {
            player.followWithoutSong(user: friend.name, player: friend.player, key: friend.key, song: friend.title, artist: friend.leadArtist)
        }
    }
}
