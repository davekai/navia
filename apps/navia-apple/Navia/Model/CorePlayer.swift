import AppKit
import Foundation
import MediaPlayer
import NaviaCoreFFI
import Observation

private nonisolated final class CorePlayerEvents: PlayerListener, @unchecked Sendable {
    let handler: @Sendable (PlayerEventRow) -> Void
    init(_ handler: @escaping @Sendable (PlayerEventRow) -> Void) { self.handler = handler }
    func onEvent(event: PlayerEventRow) { handler(event) }
}

/// The real player. Commands go to the core, and state comes back through events.
@Observable final class CorePlayerModel: PlayerViewModel {
    /// Song rows kept for the queue's view. Past this, only rows still in the queue are kept.
    private let songCacheMax = 20000
    /// seconds a seek ignores position reports, until the engine catches up.
    private let seekSettle = 1.5

    @ObservationIgnored private(set) var library: CoreLibraryModel
    @ObservationIgnored private let core: NaviaCore?
    @ObservationIgnored private(set) var player: NaviaPlayer?
    @ObservationIgnored private var entries: [QueueEntry] = [] {
        didSet { queueVersion &+= 1; scheduleAutoplayUpdate() }
    }
    private var queueVersion = 0
    /// The queue's model. Every change goes through it, so its edits reach the player in order.
    /// `entries` and `queueIndex` mirror it for views.
    @ObservationIgnored private var queue: NaviaQueue?
    @ObservationIgnored private var queueView: QueueView?
    @ObservationIgnored private var songCache: [String: SongRow] = [:]
    /// Starts and mixes run here, in order, off the main thread.
    @ObservationIgnored private let queueWork = DispatchQueue(label: "navia.queue", qos: .userInitiated)
    @ObservationIgnored private var backgroundPending = 0
    @ObservationIgnored private var seekGuard: (ms: UInt64, at: CFAbsoluteTime)?
    @ObservationIgnored private var lastSaved = CFAbsoluteTimeGetCurrent()
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var lyricsFor: String?
    @ObservationIgnored fileprivate let nowPlaying = NowPlayingBridge()
    @ObservationIgnored private var restored = false

    private var queueIndex = 0 {
        didSet { if queueIndex != oldValue { scheduleAutoplayUpdate() } }
    }
    private(set) var isPlaying = false
    /// The position views draw. It holds still while the window is hidden (`setViewsLive`), so position reports
    /// don't redraw anything. Everything else reads `heardPosition`.
    private(set) var position: Double = 0
    /// kept current whether or not the window shows.
    @ObservationIgnored private(set) var heardPosition: Double = 0
    @ObservationIgnored private var viewsLive = true
    private(set) var lyrics: [LyricLine] = []
    private(set) var lyricsSynced = false
    private(set) var lyricsPending = false

    private(set) var currentSong: SongRow?
    private(set) var status = "stopped"
    /// the queue waits underneath.
    private(set) var station: RadioStation? {
        didSet { queue?.setHolding(on: holdsPlayer) }
    }
    /// A station or a followed song holds the player, and queue edits wait until it is released.
    private var holdsPlayer: Bool { station != nil || along != nil }
    private(set) var isWaitingForServer = false
    /// The quality badge's facts. The file is fetched once per song.
    private(set) var quality: AudioQuality?
    @ObservationIgnored private var qualityFile: (id: String, file: AudioFileRow?)?
    /// the output device's own rate, where the output can tell; set as the output opens
    @ObservationIgnored private var deviceRate: UInt32?
    /// from another client or device, offered until answered (`queue_sync.rs`).
    private(set) var queueOffer: RemoteQueueRow?
    private(set) var queueSource: PlaySource?
    /// Names the next list played, when its starter knows better than the page.
    @ObservationIgnored var nextSource: PlaySource?
    @ObservationIgnored var sourceProvider: (() -> PlaySource?)?
    @ObservationIgnored private var lastEqPlan: EqPlan??

    var current: SongRow? {
        guard let currentSong else { return nil }
        return library.collection.apply(currentSong)
    }

    /// Updates the badge's facts. The file is read once per song, and the playback path is read with each
    /// position report.
    private func refreshQuality() {
        guard station == nil, let song = currentSong, let core else {
            if quality != nil { quality = nil }
            return
        }
        if qualityFile?.id != song.id {
            // A song that didn't come from a library list (radio, Autoplay, a restored queue) has no row number yet.
            let rowid = song.rowid ?? core.songsById(ids: [song.id]).first?.rowid
            qualityFile = (song.id, rowid.flatMap { core.audioFile(rowid: $0) })
        }
        guard let file = qualityFile?.file else {
            if quality != nil { quality = nil }
            return
        }
        let delivery = player?.delivery()
        let tier: AudioQuality.Tier = switch file.tier {
        case .hiRes: .hiRes
        case .lossless: .lossless
        case .lossy: .lossy
        }
        let source: AudioQuality.Source? = delivery.map {
            switch $0.source {
            case .stream: .stream
            case .cache: .cache
            case .download: .download
            }
        }
        // a bitrate of 0 leaves it to the server, so it's unknown.
        let next = AudioQuality(format: file.format, tier: tier, kbps: file.kbps.map(Int.init), bitDepth: file.bitDepth.map(Int.init), sampleRate: file.sampleRate.map(Int.init),
                                channels: file.channels.map(Int.init), source: source, transcodeFormat: delivery?.transcodeFormat, transcodeKbps: delivery?.transcodeKbps.flatMap { $0 > 0 ? Int($0) : nil },
                                outputRate: deviceRate.map(Int.init))
        if next != quality { quality = next }
    }

    var featuredArtists: [FeaturedArtist] { currentSong?.credits ?? [] }

    var shuffle: Bool {
        didSet { if shuffle != oldValue { reshuffle() } }
    }

    var repeatMode: PlayerRepeat {
        didSet {
            UserDefaults.standard.set(repeatMode.rawValue, forKey: "repeatMode")
            UserDefaults.standard.set(repeatMode != .off, forKey: "repeat")
            applyRepeat()
            publishModes()
            scheduleAutoplayUpdate()
        }
    }

    var shuffleStyle: ShuffleStyle {
        didSet { UserDefaults.standard.set(shuffleStyle == .standard ? "standard" : "fewer", forKey: "shuffleStyle") }
    }

    /// off keeps the system's output rate, which every app shares.
    var matchSampleRate: Bool {
        didSet {
            UserDefaults.standard.set(matchSampleRate ? "follow" : "device", forKey: "outputPolicy")
            player?.setOutputPolicy(policy: matchSampleRate ? .followTrack : .deviceDefault)
        }
    }

    var autoplayEnabled: Bool {
        didSet {
            UserDefaults.standard.set(autoplayEnabled, forKey: AccountPrefs.key("autoplay"))
            scheduleAutoplayUpdate()
        }
    }

    /// Settings > Playback > Autoplay mode, kept per account by the core.
    var autoplayMode: AutoplayModeRow = .navia {
        didSet {
            guard autoplayMode != oldValue, let core else { return }
            do { try core.setAutoplayMode(mode: autoplayMode) } catch { NSLog("navia: couldn't save the Autoplay mode: \(error)") }
            scheduleAutoplayUpdate()
        }
    }

    var privateMode: Bool {
        didSet {
            UserDefaults.standard.set(privateMode, forKey: AccountPrefs.key("privateMode"))
            core?.setPrivateMode(private: privateMode)
            publishDiscord()
            listen?.refreshActive()
        }
    }

    /// Settings > Share what I'm playing. Off by default. When on, playback reports carry the share mark and
    /// Listening Now shows, unless Private mode is on.
    var shareListening: Bool {
        didSet {
            UserDefaults.standard.set(shareListening, forKey: "shareListening")
            core?.setSharing(on: shareListening)
            listen?.refreshActive()
        }
    }

    /// Listening Now and its checks.
    @ObservationIgnored private(set) var listen: ListenAlongModel?

    /// Who is being followed, on which player, and their song as the server last reported it.
    /// Their song plays in place of the queue, which waits.
    struct Along {
        let user: String
        var player: String
        /// Their song as it plays here. Nil while a Spotify friend's song isn't in the library.
        var entry: NowPlayingRow?
        let back: Double
        /// A friend's song isn't in the library, so nothing plays until they pick one that is.
        var waiting = false
        /// The listener paused it, so it stays paused and following until they play again.
        var held = false
        /// When the listener last changed playback while following.
        var ownChangeMs: UInt64?
        var seen: FollowSeen?
        /// the listener chose this song or place; the host comes to it.
        var lead = false
        /// "Sending to <user>…" while the host hasn't taken the listener's change.
        var sending = false
        var twoWaySince: UInt64?
    }

    func followLocal() -> FollowLocal? {
        guard let along else { return nil }
        return FollowLocal(songId: along.entry?.song.id ?? "", positionMs: UInt64(max(0, heardPosition) * 1000), playing: isPlaying,
                           held: along.held, ownChangeMs: along.ownChangeMs, lead: along.lead, seen: along.seen, ended: status == "stopped",
                           viewer: core?.username() ?? "", twoWaySince: along.twoWaySince, nowMs: listenNowMs())
    }

    func noteSeen(_ seen: FollowSeen?, twoWay: Bool) {
        guard var current = along else { return }
        current.seen = seen
        current.twoWaySince = twoWay ? (current.twoWaySince ?? listenNowMs()) : nil
        along = current
    }

    /// When this user last changed playback. With listener control, the last change wins.
    @ObservationIgnored private(set) var hostChangeMs: UInt64 = 0
    /// the listener whose song or place this host took, while it plays it.
    @ObservationIgnored private var led: LedRow?
    private func noteHostChange() {
        hostChangeMs = listenNowMs()
        led = nil
    }

    /// from asking the player to start a song mid-way to it playing, measured on the last start.
    @ObservationIgnored private var startLatencyMs: UInt64 = 0
    @ObservationIgnored private var startingSince: CFAbsoluteTime?

    /// where a song started mid-way should start so it lands where the other side is by then.
    private func catchUpMs(_ ms: UInt64, playing: Bool) -> UInt64 { playing && ms > 0 ? ms + startLatencyMs : ms }

    // MARK: Listening session

    /// who plays the next session song at a song's end.
    @ObservationIgnored let handover = SessionHandover()
    /// the player ends after the playing song: the other side's session song comes next.
    @ObservationIgnored private var sessionHolding = false

    var sessionOn: Bool { listen?.sessionOn ?? false }
    var sessionTheirs: [String] { listen?.sessionTheirs ?? [] }
    /// this side's session songs are its Next in Queue (the waiting queue's while following).
    var sessionMine: Bool { (queueView?.queuedRun ?? 0) > 0 }
    var sessionLeftMs: UInt64? {
        guard let song = currentSong, song.duration > 0 else { return nil }
        return UInt64(max(0, Double(song.duration) * 1000 - heardPosition * 1000))
    }

    /// each check: the turn for the coming end, the host's hold while it's the other side's, and the bounded wait.
    func sessionCheck() {
        guard sessionOn else {
            setSessionHold(false)
            handover.reset()
            return
        }
        let host = along == nil
        if !handover.waiting() {
            let turn = handover.plan(mine: sessionMine, theirs: !sessionTheirs.isEmpty, host: host)
            if host { setSessionHold(turn == .theirs) }
        }
        if handover.waitOver(), host {
            // their song didn't come: the host's own queue carries on
            setSessionHold(false)
            player?.next()
        }
    }

    private func setSessionHold(_ on: Bool) {
        guard sessionHolding != on else { return }
        sessionHolding = on
        player?.setHoldAfterCurrent(on: on)
    }

    /// the host's song ended while holding for the other side's song: wait (bounded), or carry on.
    private func sessionQueueEnded() {
        if sessionOn, handover.atEnd(mine: sessionMine, theirs: !sessionTheirs.isEmpty, host: true) == .theirs {
            listen?.checkNow()
            return
        }
        setSessionHold(false)
        player?.next()
    }

    /// the host moved to the next song: its own session song, when that was the turn.
    private func sessionTrackChanged() {
        guard along == nil, sessionOn else { return }
        setSessionHold(false)
        if queueView?.currentQueued == true, handover.turn() == .mine, let id = currentSong?.id {
            handover.startedMine(songId: id)
        } else if !handover.waiting() {
            handover.songChanged()
        }
    }

    /// the followed song ended here: on this side's turn its first queued song plays, and the host takes it.
    private func sessionFollowedEnded() {
        guard sessionOn, handover.atEnd(mine: sessionMine, theirs: !sessionTheirs.isEmpty, host: false) == .mine else { return }
        guard let id = queue?.takeQueuedNext() else { return }
        mirror()
        guard let row = core?.songById(id: id), alongPlay(SongRow(core: row)) else { return }
        handover.startedMine(songId: id)
    }

    /// a session song of this side's that didn't get played goes back to the front of its queue.
    private func sessionPutBack(_ id: String) {
        guard let row = core?.songById(id: id) else { return }
        queueSongs([SongRow(core: row)], at: 0, queued: true)
    }

    /// the listener's change waits for a host that lets listeners control it.
    private func markOwnChange(lead: Bool) {
        guard var current = along else { return }
        let controls = alongControls
        current.ownChangeMs = controls ? listenNowMs() : nil
        current.sending = controls
        current.lead = controls && (lead || current.lead)
        along = current
    }

    /// Whether the followed person lets listeners control playback.
    var alongControls: Bool { along.map { lanyardFriendOf(player: $0.player) == nil && allowsControl(player: $0.player) } ?? false }

    /// Play or pause while following. A pause holds, and play catches up with the host at once.
    /// If the host allows listener control, their Navia mirrors it.
    private func alongToggle(_ play: Bool? = nil) -> Bool {
        guard var current = along, let player else { return false }
        let play = play ?? !isPlaying
        current.held = !play
        along = current
        // only a host that lets listeners control playback mirrors it; otherwise play catches up with the host at once
        markOwnChange(lead: false)
        if play { player.play() } else { player.pause() }
        isPlaying = play
        if play { listen?.checkNow() }
        return true
    }

    /// Seeks while following a host that allows listener control. Their Navia mirrors it.
    private func alongSeek(_ ms: UInt64) -> Bool {
        guard alongControls, along != nil, let player else { return false }
        markOwnChange(lead: true)
        player.seekMs(ms: ms)
        setPosition(Double(ms) / 1000)
        return true
    }

    /// Plays a song while following a host that allows listener control. It replaces theirs here and on their Navia.
    private func alongPlay(_ song: SongRow) -> Bool {
        guard alongControls, var current = along, let row = core?.songById(id: song.id) else { return false }
        let entry = NowPlayingRow(user: current.user, player: current.player, song: row, state: .playing, positionMs: 0, canListenAlong: true)
        current.entry = entry
        current.held = false
        along = current
        markOwnChange(lead: true)
        loadFollowed(entry)
        return true
    }

    /// Skips while following a host that allows listener control. Next plays the listener's own waiting song, and
    /// Previous restarts the song.
    private func alongSkip(forward: Bool) -> Bool {
        guard alongControls else { return false }
        guard forward else { return alongSeek(0) }
        guard entries.indices.contains(queueIndex), alongPlay(entries[queueIndex].song) else { return false }
        // The queue waits under the followed song, and its place moves past the song played.
        edit { $0.stepWaiting() }
        return true
    }

    /// when this Navia isn't following anyone.
    func applyHostMirror(_ mirror: HostMirror) {
        guard let player, along == nil else { return }
        led = mirror.led
        let seek = { (ms: UInt64) in
            player.seekMs(ms: ms)
            self.setPosition(Double(ms) / 1000)
        }
        switch mirror.action {
        case .pause(let place):
            if let place { seek(place) }
            player.pause()
        case .play(let place):
            if let place { seek(place) }
            player.play()
        case .seek(let ms): seek(ms)
        case .song(let id, let ms, let playing):
            guard let row = core?.songById(id: id) else { return }
            let clash = handover.theirsArrived()
            setSessionHold(false)
            // it plays now, as a queued song; the host's queue carries on after it
            playNext(SongRow(core: row))
            startingSince = CFAbsoluteTimeGetCurrent()
            player.next()
            let at = catchUpMs(ms, playing: playing)
            if at > 0 { player.seekMs(ms: at) }
            if playing { player.play() } else { player.pause() }
            // both sides started a session song at once: the listener's plays, the host's waits next
            if let clash { sessionPutBack(clash) }
        }
    }

    func hostRow() -> HostRow? {
        guard along == nil, let song = currentSong, station == nil else { return nil }
        return HostRow(songId: song.id, positionMs: UInt64(max(0, heardPosition) * 1000), playing: isPlaying, ownChangeMs: hostChangeMs,
                       led: led?.songId == song.id ? led : nil)
    }
    private(set) var along: Along? {
        didSet { if (along == nil) != (oldValue == nil) { queue?.setHolding(on: holdsPlayer) } }
    }
    /// The followed user's name, or a Spotify friend's name. `followingFriend` tells them apart.
    var followingUser: String? { along?.user }
    var followingFriend: String? { along.flatMap { lanyardFriendOf(player: $0.player) } }
    var followingWaiting: Bool { along?.waiting ?? false }
    var followingHeld: Bool { along?.held ?? false }
    var followingSending: Bool { along?.sending ?? false }
    var waitingSong: SongRow? {
        _ = queueVersion
        return along != nil && entries.indices.contains(queueIndex) ? entries[queueIndex].song : nil
    }
    /// Where the listener's own song was, so it can resume after following.
    @ObservationIgnored private var resumeAfterAlong: (index: Int, id: String, seconds: Double)?

    /// The slider's volume, 0 to 100. It is the AirPlay receiver's volume while that is the output, and this Mac's
    /// saved volume otherwise.
    var volume: Double {
        get { deviceVolume ?? localVolume }
        set {
            if deviceVolume != nil { deviceVolume = newValue } else { localVolume = newValue }
            applyVolume()
        }
    }
    /// This Mac's own volume, 0 to 100. A receiver's level is never saved here.
    private var localVolume: Double { didSet { UserDefaults.standard.set(localVolume, forKey: "volume") } }
    /// The AirPlay receiver's volume, 0 to 100, while it is the output. Never saved.
    private var deviceVolume: Double? = nil

    var isMuted: Bool {
        didSet {
            UserDefaults.standard.set(isMuted, forKey: "muted")
            applyVolume()
        }
    }

    init(library: CoreLibraryModel) {
        self.library = library
        core = library.core
        let defaults = UserDefaults.standard
        localVolume = defaults.object(forKey: "volume") as? Double ?? 80
        isMuted = defaults.bool(forKey: "muted")
        shuffle = defaults.bool(forKey: "shuffle")
        repeatMode = PlayerRepeat.saved(defaults)
        privateMode = defaults.bool(forKey: AccountPrefs.key("privateMode"))
        autoplayEnabled = defaults.object(forKey: AccountPrefs.key("autoplay")) as? Bool ?? true
        autoplayMode = core?.autoplayMode() ?? .navia
        shareListening = defaults.bool(forKey: "shareListening")
        shuffleStyle = defaults.string(forKey: "shuffleStyle") == "standard" ? .standard : .fewerRepeats
        matchSampleRate = defaults.string(forKey: "outputPolicy") != "device"
        guard let core else { return }
        autoplayCore = core.makeAutoplay()
        let policy: OutputPolicy = matchSampleRate ? .followTrack : .deviceDefault
        do {
            // AirPlay receivers are listed with the Mac's devices, except in test and capture runs.
            if let airplay = AirPlayModel.shared.startService(core: core) {
                player = try core.makePlayerWithAirplay(policy: policy, airplay: airplay)
            } else {
                player = try core.makePlayer(policy: policy, capture: defaults.bool(forKey: "naviaCapture"))
            }
        } catch {
            Log.playback.error("no audio output: \(String(describing: error), privacy: .public)")
            return
        }
        if let player { queue = core.makeQueue(player: player, autoplay: autoplayCore) }
        applyVolume()
        applyRepeat()
        publishModes()
        core.setPrivateMode(private: privateMode)
        player?.setListener(listener: CorePlayerEvents { [weak self] event in
            Task { @MainActor in self?.handle(event) }
        })
        nowPlaying.attach(self)
        restoreSession()
        if shareListening { core.setSharing(on: true) }
        listen = ListenAlongModel(core: core, player: self)
        checkQueueOffer()
        NotificationCenter.default.addObserver(forName: NSApplication.willTerminateNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.saveSessionNow()
                self?.syncBeforeQuit()
            }
        }
    }

    /// runs before the next account's models are made.
    func shutdown() {
        generation &+= 1
        saveSessionNow()
        syncBeforeQuit()
        restored = false
        bufferingWatch?.cancel()
        bufferingWatch = nil
        nowPlaying.detach()
        nowPlaying.update(song: nil, isPlaying: false, position: 0)
        setDiscordStatus(false)
        let old = player
        player = nil
        Task.detached(priority: .utility) {
            old?.stop()
            // Dropping the player joins its threads, so it happens here and not on the main thread.
            withExtendedLifetime(old) {}
        }
    }

    // MARK: Discord

    @ObservationIgnored private let discord = makeDiscordStatus()
    @ObservationIgnored private var discordOn = false

    /// never in a test run.
    func setDiscordStatus(_ on: Bool) {
        let enable = on && !CoreMode.isTestRun
        guard enable != discordOn else { return }
        discordOn = enable
        let (discord, core) = (discord, core)
        Task.detached(priority: .utility) { [weak self] in
            discord.setServer(core: enable ? core : nil)
            discord.setEnabled(on: enable)
            await MainActor.run { self?.publishDiscord() }
        }
    }

    /// Settings > Discord "Public server address". The core checks that it is a usable https address.
    func setDiscordCoversFrom(_ value: String) {
        discord.setCoverSource(source: value == "server" ? .server : value == "search" ? .search : .automatic)
    }

    func setDiscordPublicAddress(_ text: String) {
        discord.setPublicAddress(text: text)
    }

    private func publishDiscord() {
        guard discordOn else { return }
        let track = currentSong.map { DiscordTrackRow(id: $0.id, title: $0.title, artist: $0.artist, album: $0.album, durationMs: $0.duration > 0 ? UInt64($0.duration) * 1000 : nil, albumId: $0.albumID.isEmpty ? nil : $0.albumID) }
        discord.update(track: track, playing: isPlaying && !isWaitingForServer, positionMs: UInt64(max(0, heardPosition) * 1000), privateMode: privateMode)
    }

    /// Writes synchronously, because the process is about to exit.
    private func saveSessionNow() {
        guard restored, entries.indices.contains(queueIndex) else { return }
        let saved = savedSession
        if station == nil {
            let own = max(queueIndex + 1, entries.count - pendingAutoCount)
            core?.noteQueue(songIds: entries.prefix(own).map(\.song.id), index: UInt32(queueIndex), positionMs: saved.positionMs)
        }
        guard let url = sessionURL, let data = try? JSONEncoder().encode(saved) else { return }
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? data.write(to: url)
        if let queue, let path = queueFileURL?.path { _ = queue.save(path: path) }
    }

    /// Saves the queue on the server and ends this player's now-playing entry. Waits at most about two seconds,
    /// since the process is exiting or the account is changing.
    private func syncBeforeQuit() {
        guard let core else { return }
        let done = DispatchSemaphore(value: 0)
        Task.detached(priority: .userInitiated) {
            await core.syncBeforeQuit()
            done.signal()
        }
        _ = done.wait(timeout: .now() + 2.5)
    }

    // MARK: Another device's queue

    var queueOfferDevice: String? { queueOffer.map { clientDisplayName(name: $0.device) } }

    /// Asks once after launch whether the server holds a newer queue from another device. The core waits for the
    /// connection and the restored queue first. Read-only.
    private func checkQueueOffer() {
        guard let core else { return }
        let generation = generation
        Task { [weak self] in
            let offer = await Task.detached(priority: .utility) { await core.remoteQueueOffer() }.value
            guard let self, self.generation == generation, let offer, !offer.songs.isEmpty else { return }
            self.queueOffer = offer
        }
    }

    /// Continue loads the other device's queue, paused at its place. Not now keeps this one.
    func answerQueueOffer(accept: Bool) {
        guard let offer = queueOffer else { return }
        queueOffer = nil
        core?.answerQueueOffer()
        guard accept, player != nil else { return }
        dropAlong()
        if station != nil { leaveStation() }
        let rows = offer.songs.map { SongRow(core: $0) }
        let index = min(Int(offer.index), rows.count - 1)
        guard let queue, index >= 0 else { return }
        generation &+= 1
        setSource(nil)
        remember(rows)
        queue.replacePlain(songs: rows.map(Self.item), pos: UInt64(index), queuedStart: 0, queuedEnd: 0, shuffle: shuffle)
        edit { $0.load(startMs: offer.positionMs, paused: true) }
        setPosition(Double(offer.positionMs) / 1000)
        isPlaying = false
        status = "paused"
        setCurrent(rows[index])
        publishNowPlaying()
        saveSession()
    }

    /// Shows the AirPlay receiver's volume, 0 to 1, while it is the output. Nil when the output is this Mac again.
    /// Nothing is sent back.
    func showDeviceVolume(_ level: Float?) {
        deviceVolume = level.map { (Double($0.squareRoot()) * 100).rounded() }
    }

    private func applyVolume() {
        let v = min(1, max(0, volume / 100))
        player?.setVolume(volume: Float(v * v))
        player?.setMuted(muted: isMuted)
    }

    private func applyRepeat() {
        let mode: RepeatMode = switch repeatMode { case .off: .off; case .all: .all; case .one: .one }
        player?.setRepeat(mode: mode)
    }

    private func publishModes() { nowPlaying.setModes(repeatMode: repeatMode, shuffle: shuffle) }

    // MARK: Events

    @ObservationIgnored private var songBeforeStation: SongRow?

    private func setPosition(_ value: Double) {
        heardPosition = value
        if viewsLive, position != value { position = value }
    }

    /// Turns position updates to views off while the window is hidden. Turning it on pushes the current position.
    /// Audio, Now Playing, Discord and the saved session don't depend on this.
    func setViewsLive(_ live: Bool) {
        guard live != viewsLive else { return }
        viewsLive = live
        if live, position != heardPosition { position = heardPosition }
    }

    private func handle(_ event: PlayerEventRow) {
        switch event {
        case .trackChanged(let index, let item, let left):
            // anything else means a song took over and the station ended.
            if item.id == liveItemID { return }
            if along != nil { return }
            if station != nil { station = nil; liveItemID = nil; liveTitle = nil; songBeforeStation = nil }
            // The model follows the change, the rest of the list feeds in when it runs low, and the player then counts
            // its indexes without the song that left.
            queue?.trackChanged(index: index, leftIndex: left.map(\.index), leftSeq: left.map(\.seq))
            mirror()
            if let resume = resumeAfterAlong {
                resumeAfterAlong = nil
                if resume.index == queueIndex, resume.id == item.id, resume.seconds > 0 { player?.seekMs(ms: UInt64(resume.seconds * 1000)) }
            }
            if entries.indices.contains(queueIndex), entries[queueIndex].song.id == item.id {
                setCurrent(entries[queueIndex].song)
            } else if let song = core?.songById(id: item.id).map({ SongRow(core: $0) }) {
                setCurrent(song)
            }
            setPosition(Double(player?.quickPositionMs() ?? 0) / 1000)
            queueEnded = false
            if let song = currentSong { autoplayCore?.heard(song: Self.autoplaySong(song)) }
            sessionTrackChanged()
            publishNowPlaying()
            saveSession()
        case .statusChanged(let status):
            switch status {
            case .playing:
                isPlaying = true; self.status = "playing"
                if let since = startingSince {
                    startingSince = nil
                    startLatencyMs = min(UInt64((CFAbsoluteTimeGetCurrent() - since) * 1000), listenAlongTiming().resyncDriftMs)
                }
            case .buffering: isPlaying = true; self.status = "buffering"
            case .paused:
                isPlaying = false; self.status = "paused"
                // A pause is a good moment to sync the server's copy of the queue.
                if let core { Task.detached(priority: .utility) { await core.flushQueue() } }
            case .stopped: isPlaying = false; self.status = "stopped"
            }
            watchBuffering()
            publishNowPlaying()
            saveSession()
        case .position(let ms):
            if let guardPos = seekGuard {
                if abs(Int64(ms) - Int64(guardPos.ms)) > 900, CFAbsoluteTimeGetCurrent() - guardPos.at < seekSettle { return }
                seekGuard = nil
            }
            setPosition(Double(ms) / 1000)
            refreshQuality()
            if station != nil { updateLiveTitle() }
            if CFAbsoluteTimeGetCurrent() - lastSaved > 5 { saveSession() }
        case .queueEnded where sessionHolding:
            isPlaying = false
            status = "stopped"
            sessionQueueEnded()
        case .queueEnded where along != nil:
            // The followed song ended here. Ask the server now so the next one follows at once.
            isPlaying = false
            status = "stopped"
            sessionFollowedEnded()
            listen?.checkNow()
        case .queueEnded:
            // Autoplay only starts a late answer if it was on when the list ran out.
            queueEnded = autoplayAllowed
            scheduleAutoplayUpdate()
            isPlaying = false
            status = "stopped"
            publishNowPlaying()
        case .connection(let waiting, _):
            isWaitingForServer = waiting
            library.noteNetworkTrouble()
            publishNowPlaying()
        case .error(let message):
            Log.playback.error("playback: \(message, privacy: .public)")
            let stationName = station?.name
            if stationName != nil { leaveStation() }
            library.noteNetworkTrouble()
            noticeError(stationName.map { L("{name} stopped playing", ["name": $0]) } ?? L("Couldn't play this song"))
        case .outputError(let device, let message):
            // The error is about the output device, so name it when known.
            Log.playback.error("output \(device ?? "default", privacy: .public): \(message, privacy: .public)")
            noticeError(device.map { L("{device} couldn't start", ["device": $0]) } ?? L("The output device couldn't start"))
        case .repeatChanged(let mode):
            // Next or Previous during Repeat one switches the player to Repeat all.
            let next: PlayerRepeat = switch mode { case .off: .off; case .all: .all; case .one: .one }
            if repeatMode != next { repeatMode = next }
        case .outputChanged(_, let rate, _):
            deviceRate = rate
            refreshQuality()
        case .queueChanged:
            break
        }
    }

    /// Buffering this long usually means the server went away, so ask rather than spin.
    @ObservationIgnored private var bufferingWatch: Task<Void, Never>?
    @ObservationIgnored private var lastErrorNotice = CFAbsoluteTime.zero

    private func watchBuffering() {
        bufferingWatch?.cancel()
        bufferingWatch = nil
        guard status == "buffering" else { return }
        bufferingWatch = Task { [weak self] in
            try? await Task.sleep(for: .seconds(8))
            guard !Task.isCancelled, let self, self.status == "buffering" else { return }
            self.library.noteNetworkTrouble()
        }
    }

    /// Shows an error without flooding the screen. The Offline row covers the server going away.
    private func noticeError(_ text: String) {
        let now = CFAbsoluteTimeGetCurrent()
        guard !library.isOffline, now - lastErrorNotice > 6 else { return }
        lastErrorNotice = now
        library.onNotice?(text)
    }

    func retryServer() { player?.retryNow() }

    /// When offline, only what's on this device plays.
    private func refusedOffline(_ song: SongRow?) -> Bool {
        guard let song, library.isOffline, !library.playsOffline(song: song) else { return false }
        library.onNotice?(L("This song isn't downloaded. It needs the server."))
        return true
    }

    private func setCurrent(_ song: SongRow) {
        guard song.id != currentSong?.id else {
            if currentSong?.artworkURL != song.artworkURL { currentSong = song }
            return
        }
        currentSong = song
        refreshQuality()
        lyrics = []
        lyricsSynced = false
        loadLyrics(for: song)
        loadTint(for: song)
    }

    private func loadTint(for song: SongRow) {
        guard let core, let key = song.artworkURL.map({ _ in song.albumID }), !key.isEmpty else { return }
        let id = song.id
        Task.detached(priority: .utility) { [weak self] in
            let cover = try? await core.cover(key: key, size: 512)
            let hex = cover.map { $0.tintRgb.prefix(3).map { String(format: "%02x", $0) }.joined() }
            await MainActor.run {
                guard let self, let hex, self.currentSong?.id == id, var song = self.currentSong else { return }
                song.tintHex = hex
                self.currentSong = song
                if let path = cover?.path { self.nowPlaying.setArtwork(path: path, songID: id) }
            }
        }
    }

    private func loadLyrics(for song: SongRow) {
        guard let core else { return }
        let id = song.id
        lyricsFor = id
        lyricsPending = true
        Task.detached(priority: .utility) { [weak self] in
            let found = try? await core.lyrics(songId: id, artist: song.artist, title: song.title)
            await MainActor.run {
                guard let self, self.lyricsFor == id else { return }
                self.lyricsPending = false
                let lines = found?.lines.map { row in
                    LyricLine(start: row.startMs.map(Int.init), text: row.text, endMs: row.endMs.map(Int.init),
                              words: row.words.map { LyricWord(start: $0.start, end: $0.end, startMs: $0.startMs, endMs: $0.endMs) })
                } ?? []
                self.lyrics = lines
                self.lyricsSynced = found?.synced ?? false
                guard !lines.isEmpty, var song = self.currentSong, song.id == id else { return }
                song.hasLyrics = true
                self.currentSong = song
            }
        }
    }

    // MARK: Playback

    func togglePlayPause() {
        guard current != nil else { return }
        if alongToggle() { return }
        noteHostChange()
        takeControlFromAlong()
        player?.toggle()
    }

    private func updateLiveTitle() {
        guard let player, let station else { return }
        let title = player.streamTitle()
        guard title != liveTitle else { return }
        liveTitle = title
        guard var song = currentSong, song.id == liveItemID else { return }
        song = SongRow(id: song.id, title: station.name, artist: title ?? L("Internet radio"), album: "", albumID: "", duration: 0, genre: "", year: nil, plays: 0, isFavorite: false, isDownloaded: false, artworkURL: nil, tintHex: nil)
        currentSong = song
        publishNowPlaying()
    }

    /// Skip, previous, seek and play or pause end following first.
    func next() {
        if alongSkip(forward: true) { return }
        noteHostChange()
        takeControlFromAlong()
        player?.next()
    }

    func previous() {
        if alongSkip(forward: false) { return }
        noteHostChange()
        takeControlFromAlong()
        player?.previous()
    }

    // MARK: Listen along

    /// Starts following `entry`'s user after the confirm. Their song plays here from the same moment, and the
    /// queue waits.
    func follow(_ entry: NowPlayingRow) {
        guard entry.canListenAlong, player != nil else { return }
        let back = ownPosition()
        if station != nil { station = nil; liveItemID = nil; liveTitle = nil; songBeforeStation = nil }
        along = Along(user: entry.user, player: entry.player, entry: entry, back: back)
        loadFollowed(entry)
    }

    /// Follows a Spotify friend whose song isn't in the library. The listener's song pauses, a toast says so, and
    /// their next song that is in the library plays.
    func followWithoutSong(user: String, player name: String, key: String, song: String, artist: String) {
        guard player != nil else { return }
        let back = ownPosition()
        along = Along(user: user, player: name, entry: nil, back: back, waiting: true)
        // The confirm already said this song isn't here, so the chip shows the wait without a toast.
        skippedKey = key
        skipMissing(key: key, song: song, artist: artist)
    }

    /// Where the listener's own song is, to resume from when following ends. Switching hosts keeps the position
    /// saved earlier.
    private func ownPosition() -> Double {
        if let prior = along { return prior.back }
        return station == nil ? heardPosition : 0
    }

    @ObservationIgnored private var skippedKey: String?

    /// Pauses when a followed friend's song isn't in the library. The toast shows once per song, and following
    /// goes on.
    private func skipMissing(key: String, song: String, artist: String) {
        if isPlaying { player?.pause(); isPlaying = false; status = "paused" }
        if along?.waiting == false { along?.waiting = true }
        guard skippedKey != key else { return }
        skippedKey = key
        library.onNotice?(L("“{song}” by {artist} isn’t in your library, so it was skipped.", ["song": song, "artist": artist]))
    }

    func applyFollowStep(_ step: FollowStep) {
        guard var current = along, let player else { return }
        switch step {
        case .ended:
            settleAlong(pause: true)
            listen?.followingEnded(user: current.user)
        case .keep(_, let name):
            current.player = name
            current.ownChangeMs = nil
            current.sending = false
            along = current
        case .sending(let name):
            current.player = name
            current.sending = true
            along = current
        case .lead(let user):
            // they listen along with this Navia now; it keeps playing as their host
            stopFollowing()
            library.onNotice?(L("{user} is listening along with you", ["user": user]))
        case .notInLibrary(let key, let song, let artist):
            skipMissing(key: key, song: song, artist: artist)
        case .notTaken(let entry, let name):
            let mine = currentSong?.id
            applyFollowStep(.resync(entry: entry, player: name))
            if let mine, handover.theirsArrived() == mine { sessionPutBack(mine) }
            library.onNotice?(L("{user} didn’t take your change", ["user": current.user]))
        case .resync(let entry, let name):
            let sameSong = entry.song.id == current.entry?.song.id
            current.player = name
            current.entry = entry
            current.waiting = false
            current.held = false
            current.ownChangeMs = nil
            current.lead = false
            current.sending = false
            along = current
            guard sameSong else {
                _ = handover.theirsArrived()
                loadFollowed(entry)
                return
            }
            // the follower's own moves go straight to the player, not through the controls that end following
            let playing = entry.state.map { $0 != .paused } ?? false
            let at = catchUpMs(entry.positionMs ?? 0, playing: playing)
            if abs(heardPosition * 1000 - Double(at)) > Double(listenAlongTiming().resyncDriftMs) {
                player.seekMs(ms: at)
                setPosition(Double(at) / 1000)
            }
            if playing != isPlaying { if playing { player.play() } else { player.pause() } }
        }
    }

    /// Plays the followed song alone in the player. The queue stays as it is.
    private func loadFollowed(_ entry: NowPlayingRow) {
        guard let player else { return }
        let song = SongRow(core: entry.song)
        let playing = entry.state.map { $0 != .paused } ?? false
        let at = catchUpMs(entry.positionMs ?? 0, playing: playing)
        startingSince = CFAbsoluteTimeGetCurrent()
        // no crossfade while following, so the followed song changes cut in at once.
        player.setFollowing(on: true)
        player.setQueue(items: [Self.item(song)], startIndex: 0, startMs: at, paused: !playing)
        setCurrent(song)
        setPosition(Double(at) / 1000)
        isPlaying = playing
        status = playing ? "buffering" : "paused"
        publishNowPlaying()
    }

    /// The listener took control. Following ends with a toast, and the followed song joins the queue just before
    /// the listener's own song. Returns true when following was on.
    @discardableResult func takeControlFromAlong() -> Bool {
        guard let user = along?.user else { return false }
        settleAlong(pause: false)
        library.onNotice?(L("Stopped listening along with {user}", ["user": user]))
        return true
    }

    /// Something else starts playing. Following ends with a toast, and the followed song isn't kept.
    private func dropAlong() {
        guard let user = along?.user else { return }
        along = nil
        player?.setFollowing(on: false)
        resumeAfterAlong = nil
        library.onNotice?(L("Stopped listening along with {user}", ["user": user]))
    }

    /// Ends following quietly, from the chip's Stop, the row's headphones, or sharing turned off.
    func stopFollowing() {
        guard along != nil else { return }
        settleAlong(pause: false)
    }

    /// Makes the followed song the current queue entry, just before the listener's own song, which then resumes
    /// where it was. The player keeps playing without a break.
    private func settleAlong(pause: Bool) {
        guard let current = along, let player else { return }
        along = nil
        skippedKey = nil
        // The listener's queue crossfades again. The followed song still ends with a cut.
        player.setFollowing(on: false)
        if pause {
            player.pause()
            isPlaying = false
            status = "paused"
        }
        guard let followed = current.entry else {
            // Nothing of theirs is kept, so the queue comes back as it was, paused at its song.
            resumeAfterAlong = nil
            guard !entries.isEmpty else { return }
            edit { $0.load(startMs: UInt64(max(0, current.back) * 1000), paused: true) }
            if entries.indices.contains(queueIndex) { setCurrent(entries[queueIndex].song) }
            setPosition(current.back)
            isPlaying = false
            status = "paused"
            publishNowPlaying()
            return
        }
        // The player holds only the followed song, so the queue is rebuilt around it.
        let own = entries.indices.contains(queueIndex) ? entries[queueIndex].song.id : nil
        let song = SongRow(core: followed.song)
        remember([song])
        edit { $0.keepInFront(song: Self.item(song)) }
        resumeAfterAlong = own.map { (queueIndex + 1, $0, current.back) }
        saveSession()
    }

    /// Media keys ask to play or pause outright, while the buttons toggle.
    func playIfPaused() { if alongToggle(true) { return }; if !isPlaying { togglePlayPause() } }
    func pauseIfPlaying() { if alongToggle(false) { return }; if isPlaying { togglePlayPause() } }

    func seek(to seconds: Double) {
        guard let player else { return }
        if alongSeek(UInt64(max(0, seconds) * 1000)) { return }
        noteHostChange()
        takeControlFromAlong()
        let ms = UInt64(max(0, seconds) * 1000)
        seekGuard = (ms, CFAbsoluteTimeGetCurrent())
        setPosition(Double(ms) / 1000)
        player.seekMs(ms: ms)
        publishNowPlaying()
        saveSession()
    }

    func toggleFavorite() {
        guard let song = currentSong else { return }
        library.toggleFavorite(song: song)
    }

    func play(songAt rowIndex: Int, in songs: any SongsViewModel) {
        if let model = songs as? CoreSongsModel {
            let order = model.rowids
            guard order.indices.contains(rowIndex) else { return }
            startList(.rowids(order), at: rowIndex, first: model.song(at: rowIndex), shuffle: shuffle)
        } else {
            let rows = songs.allSongs()
            guard rows.indices.contains(rowIndex) else { return }
            startList(.rows(rows), at: rowIndex, first: rows[rowIndex], shuffle: shuffle)
        }
    }

    func play(_ songs: [SongRow], at index: Int, shuffle wanted: Bool?) {
        guard !songs.isEmpty else { return }
        if let wanted { shuffle = wanted }
        let at = min(max(0, index), songs.count - 1)
        startList(.rows(songs), at: at, first: songs[at], shuffle: shuffle)
    }

    /// The songs a queue starts from, as library ids or rows at hand.
    private enum StartList: Sendable {
        case rowids([UInt32])
        case rows([SongRow])
    }

    /// Starts `list` at `start`. Songs in Next in Queue stay next, and the rest is replaced. When shuffled, the
    /// chosen song plays at once and the mix joins it.
    private func startList(_ list: StartList, at start: Int, first: SongRow?, shuffle wantShuffle: Bool) {
        let named = nextSource
        nextSource = nil
        guard queue != nil, !refusedOffline(first) else { return }
        // When following a host that allows listener control, the chosen song plays for both.
        if let first, alongPlay(first) { return }
        noteHostChange()
        dropAlong()
        // Any song played ends a station for good.
        if station != nil { station = nil; liveItemID = nil; liveTitle = nil; songBeforeStation = nil }
        setSource(named ?? sourceProvider?())
        generation &+= 1
        if let first { setCurrent(first) }
        setPosition(0)
        isPlaying = true
        status = "buffering"
        publishNowPlaying()
        if case .rows(let rows) = list { remember(rows) }
        let style = shuffleStyle
        // Reading a whole library's ids and mixing them take a moment, so they run off the main thread in order with
        // other queue work.
        background { queue in
            switch list {
            case .rowids(let rowids): _ = queue.startRowids(rowids: rowids, index: UInt64(start), shuffled: wantShuffle)
            case .rows(let rows): queue.start(songs: rows.map(Self.item), index: UInt64(start), shuffled: wantShuffle)
            }
            if wantShuffle { _ = queue.mix(style: style) }
        }
    }

    // MARK: Queue model

    /// Runs `work` on the queue's thread, then updates the queue's view here. Song rows for the view are read
    /// there too.
    private func background(_ work: @escaping @Sendable (NaviaQueue) -> Void) {
        guard let queue, let core else { return }
        backgroundPending += 1
        queueWork.async { [weak self] in
            work(queue)
            let ids = Array(Set(queue.view().entries.map(\.songId)))
            let rows = core.songsById(ids: ids).map { SongRow(core: $0) }
            Task { @MainActor in
                guard let self else { return }
                self.backgroundPending -= 1
                self.remember(rows, fresh: false)
                self.mirror()
                self.saveSession()
            }
        }
    }

    /// Keeps song rows for the queue's view. `fresh: false` keeps rows already held, which may be newer.
    private func remember(_ songs: [SongRow], fresh: Bool = true) {
        if songCache.count > songCacheMax {
            let wanted = Set(entries.map(\.song.id))
            songCache = songCache.filter { wanted.contains($0.key) }
        }
        for song in songs where fresh || songCache[song.id] == nil { songCache[song.id] = song }
    }

    private func mirror() {
        guard let queue else { return }
        let view = queue.view()
        let missing = Array(Set(view.entries.map(\.songId).filter { songCache[$0] == nil }))
        if !missing.isEmpty, let core { remember(core.songsById(ids: missing).map { SongRow(core: $0) }) }
        let pos = Int(view.pos)
        queueView = view
        entries = view.entries.map { e in
            let song = songCache[e.songId] ?? SongRow(id: e.songId, title: "", artist: "", album: "", albumID: "", duration: 0, genre: "", year: nil, plays: 0, isFavorite: false, isDownloaded: false, artworkURL: nil, tintHex: nil)
            return QueueEntry(id: Int(truncatingIfNeeded: e.id), song: song, queued: e.kind == .queued, auto: e.kind == .other)
        }
        queueIndex = entries.isEmpty ? 0 : pos
    }

    /// Runs a quick change on the queue's model here and mirrors the result.
    private func edit(_ change: (NaviaQueue) -> Void) {
        guard let queue else { return }
        change(queue)
        mirror()
    }

    nonisolated private static func item(_ song: SongRow) -> QueueItem {
        QueueItem(id: song.id, durationMs: song.duration > 0 ? UInt64(song.duration) * 1000 : nil)
    }

    private func setSource(_ source: PlaySource?) {
        queueSource = source
        player?.setSource(source: source.map(Self.coreSource))
    }

    private static func coreSource(_ s: PlaySource) -> QueueSource {
        let link: SourceLink? = switch s.link {
        case .album(let id): .album(id: id)
        case .playlist(let id): .playlist(id: id)
        case .artist(let id): .artist(id: id)
        case .artistTop(let id): .artistTop(id: id)
        case .mix(let id): .mix(id: id)
        case .liked: .liked
        case .songs(let genre): genre.map { .genre(name: $0) } ?? .songs
        default: nil
        }
        return QueueSource(name: s.name, radio: s.radio, link: link)
    }

    private static func playSource(_ s: QueueSource) -> PlaySource {
        let link: Route? = switch s.link {
        case .album(let id): .album(id)
        case .playlist(let id): .playlist(id)
        case .artist(let id): .artist(id)
        case .artistTop(let id): .artistTop(id)
        case .mix(let id): .mix(id)
        case .liked: .liked
        case .genre(let name): .songs(genre: name)
        case .songs: .songs(genre: nil)
        case nil: nil
        }
        return PlaySource(name: s.name, radio: s.radio, link: link)
    }

    var queuedCount: Int {
        _ = queueVersion
        return min(upcomingCount, Int(queueView?.queuedRun ?? 0))
    }

    /// Clear Queue. The queued songs leave the queue and the player, and the list plays on. Returns them for Undo.
    @discardableResult func clearQueued() -> [SongRow] {
        guard let queue, queuedCount > 0 else { return [] }
        let ids = queue.clearQueued()
        let songs = ids.compactMap { songCache[$0] }
        mirror()
        saveSession()
        return songs
    }

    /// Shuffle Queue. Mixes the queued songs once, in place, without changing the list or the shuffle state.
    func shuffleQueued() {
        guard queuedCount >= 2, station == nil else { return }
        edit { $0.shuffleQueued() }
        saveSession()
    }

    /// Clear <source>. Removes the rest of the list after Next in Queue and keeps Autoplay's songs. Returns an Undo
    /// that restores it if nothing changed since.
    func clearList() -> (@MainActor () -> Void)? {
        guard station == nil, let queue, queue.clearList() else { return nil }
        mirror()
        scheduleAutoplayUpdate()
        saveSession()
        return { [weak self] in
            guard let self else { return }
            self.edit { $0.undoClearList() }
            self.scheduleAutoplayUpdate()
            self.saveSession()
        }
    }

    /// Removes Up Next songs at offsets after the one playing. Removed list songs stay out when shuffle turns off.
    /// Playback stays gapless.
    func removeUpcoming(_ offsets: [Int]) {
        let gone = Set(offsets.filter { $0 >= 0 && $0 < upcomingCount }).sorted()
        guard !gone.isEmpty, station == nil else { return }
        edit { $0.remove(offsets: gone.map { UInt64($0) }) }
        saveSession()
    }

    /// The play whose song is waiting for its radio. Autoplay holds off until then.
    @ObservationIgnored private var radioPending: Int?
    private var radioWaiting: Bool { radioPending == generation }

    func playAsRadio(_ song: SongRow, radio: @escaping @MainActor () async -> [SongRow]) {
        play([song], at: 0, shuffle: false)
        guard currentSong?.id == song.id else { return }
        let generation = generation
        radioPending = generation
        Task { [weak self] in
            let songs = await radio()
            guard let self else { return }
            // The queue for the song itself lands a moment after `play`, so a quick answer waits for it.
            for _ in 0..<50 where self.backgroundPending > 0 && generation == self.generation { try? await Task.sleep(for: .milliseconds(20)) }
            if self.radioPending == generation { self.radioPending = nil }
            guard generation == self.generation, self.station == nil, !songs.isEmpty, !self.entries.isEmpty else {
                self.scheduleAutoplayUpdate()
                return
            }
            // The rest of the list goes after songs queued meanwhile and before Autoplay's songs.
            let ended = self.status == "stopped" && self.queueIndex == self.entries.count - 1
            self.remember(songs)
            self.edit { $0.appendToList(songs: songs.map(Self.item), play: ended) }
            self.saveSession()
        }
    }

    /// Play Next. Puts the song right after the one playing, ahead of earlier queued songs.
    func playNext(_ song: SongRow) {
        guard player != nil else { return }
        queueSongs([song], at: 0, queued: true)
    }

    /// Add to Queue. Puts the song after earlier queued songs, still ahead of the rest of the list.
    func addToQueue(_ song: SongRow) {
        guard player != nil else { return }
        queueSongs([song], at: queuedCount, queued: true)
    }

    /// Inserts songs at Up Next offset `at`, into Next in Queue when `queued` and into the list otherwise.
    func queueSongs(_ songs: [SongRow], at: Int, queued: Bool) {
        guard player != nil, !songs.isEmpty else { return }
        // while following, songs go into the waiting queue rather than playing
        guard !entries.isEmpty || along != nil else { play(songs, at: 0, shuffle: nil); return }
        remember(songs)
        edit { _ = $0.insert(songs: songs.map(Self.item), offset: UInt64(max(0, at)), queued: queued) }
        refreshAutoplaySongs()
        saveSession()
    }

    /// Moves songs inside Up Next to before offset `before`. Moved songs take the section they land in.
    func moveUpcoming(_ offsets: [Int], before: Int) {
        guard station == nil else { return }
        let picked = Array(Set(offsets.filter { $0 >= 0 && $0 < upcomingCount })).sorted()
        guard !picked.isEmpty, before >= 0 else { return }
        let intoQueued = before < queuedCount
        edit { $0.moveEntries(offsets: picked.map { UInt64($0) }, before: UInt64(before), intoQueued: intoQueued) }
        saveSession()
    }

    /// Internet radio isn't wired to the engine yet; nothing starts.
    func setCrossfade(ms: Int, smart: Bool, mix: Bool) {
        player?.setCrossfadeMs(ms: UInt32(ms))
        player?.setSmartCrossfade(on: smart)
        player?.setSmartTransitions(on: mix)
    }

    /// Sends the EQ chain to the player. Nil bypasses it, so off stays bit-exact.
    func applyEqualizer(_ plan: EqPlan?) {
        guard lastEqPlan != .some(plan) else { return }
        lastEqPlan = .some(plan)
        guard let plan else { player?.setEq(settings: nil); return }
        player?.setEq(settings: EqSettingsRow(preampDb: Float(plan.preampDb),
                                              bands: plan.filters.map { filter in
                                                  EqBandRow(shape: filter.kind == .lowShelf ? .lowShelf : filter.kind == .highShelf ? .highShelf : .peaking,
                                                            freqHz: Float(filter.freq), gainDb: Float(filter.gainDb), q: Float(filter.q))
                                              },
                                              limiter: plan.limiter, balance: Float(plan.balance), mono: plan.mono, crossfeed: plan.crossfeed.map { Float($0) }))
    }

    #if DEBUG
    var debugEqDescription: String {
        guard case .some(let plan) = lastEqPlan else { return "never" }
        guard let plan else { return "off" }
        return "preamp \(plan.preampDb) filters \(plan.filters.count) gains \(plan.filters.map { (String(format: "%.1fL(", $0.gainDb)) }.joined(separator: "),")) limiter \(plan.limiter) balance \(plan.balance) mono \(plan.mono)"
    }
    #endif

    @ObservationIgnored private var liveTitle: String?
    @ObservationIgnored private var liveItemID: String?
    func setOutputDevice(_ id: String?) { player?.setOutputDevice(device: id) }

    /// the song and queue stay ready underneath for when it ends.
    func playStation(_ station: RadioStation) {
        guard let player else { return }
        dropAlong()
        if self.station == station { togglePlayPause(); return }
        let itemID = "station:\(station.url)"
        liveItemID = itemID
        if self.station == nil { songBeforeStation = currentSong }
        self.station = station
        currentSong = SongRow(id: itemID, title: station.name, artist: L("Internet radio"), album: "", albumID: "", duration: 0, genre: "", year: nil, plays: 0, isFavorite: false, isDownloaded: false, artworkURL: nil, tintHex: nil)
        lyrics = []
        lyricsSynced = false
        lyricsFor = nil
        lyricsPending = false
        setPosition(0)
        isPlaying = true
        status = "buffering"
        player.playLive(item: QueueItem(id: itemID, durationMs: nil), url: station.url)
        publishNowPlaying()
    }
    func stopStation() {
        guard station != nil else { return }
        player?.stop()
        leaveStation()
    }

    private func leaveStation() {
        station = nil
        liveItemID = nil
        liveTitle = nil
        if let song = songBeforeStation { currentSong = song }
        songBeforeStation = nil
        isPlaying = false
        status = "stopped"
        setPosition(0)
        edit { $0.load(startMs: 0, paused: true) }
        publishNowPlaying()
    }

    var upcomingCount: Int {
        _ = queueVersion
        return max(0, entries.count - queueIndex - 1 - pendingAutoCount)
    }

    func upcoming(at offset: Int) -> QueueEntry? {
        let at = queueIndex + 1 + offset
        guard offset < upcomingCount, entries.indices.contains(at) else { return nil }
        var entry = entries[at]
        entry = QueueEntry(id: entry.id, song: library.collection.apply(entry.song), queued: entry.queued, auto: entry.auto)
        return entry
    }

    /// Jumps to an Up Next song. Jumping down Next in Queue drops the queued songs above it, and jumping down the
    /// list keeps them next.
    func jump(toUpcoming offset: Int) {
        // When following, the queue comes back first with the followed song in it, so the target moves down one.
        if takeControlFromAlong() { jump(toUpcoming: offset + 1); return }
        let index = queueIndex + 1 + offset
        guard entries.indices.contains(index) else { return }
        // During a station the player isn't holding this queue, so it starts again from that song.
        if station != nil {
            station = nil; liveItemID = nil; liveTitle = nil; songBeforeStation = nil
            edit { $0.restartAt(index: UInt64(index)) }
            if entries.indices.contains(queueIndex) { setCurrent(entries[queueIndex].song) }
            setPosition(0)
            isPlaying = true
            status = "buffering"
            publishNowPlaying()
            return
        }
        if refusedOffline(entries[index].song) { return }
        edit { $0.jump(index: UInt64(index)) }
    }

    /// Turns shuffle on or off mid-queue, keeping the current song. Off restores the list's order at once. On mixes
    /// the rest of the list off the main thread. Next in Queue and Autoplay's songs don't move.
    private func reshuffle() {
        UserDefaults.standard.set(shuffle, forKey: "shuffle")
        publishModes()
        guard let queue else { return }
        let on = shuffle, style = shuffleStyle
        edit { $0.setShuffle(on: on) }
        saveSession()
        guard on, queue.view().mixing else { return }
        background { queue in _ = queue.mix(style: style) }
    }

    // MARK: Autoplay

    @ObservationIgnored private var autoplayCore: NaviaAutoplay?
    /// Songs Autoplay offers that aren't in the queue yet. They show after any pending ones.
    private var autoplaySongs: [SongRow] = []
    /// Whether the offer is final. Until then the section shows none, so rows never reorder under the listener.
    private var autoplayReady = false
    @ObservationIgnored private var queueVisible = false
    @ObservationIgnored private var autoplayScheduled = false
    @ObservationIgnored private var autoplayAsking = false
    /// The player ran out of songs, so a song Autoplay adds must be started.
    @ObservationIgnored private var queueEnded = false
    /// How many Autoplay rows show.
    private static let autoplayShownMax = Int(autoplayRowsShown())
    /// How many of the context's last songs Autoplay uses as anchors.
    private static let anchorCount: UInt64 = 12

    nonisolated private static func autoplaySong(_ song: SongRow) -> AutoplaySong {
        AutoplaySong(id: song.id, title: song.title, artist: song.credits.first?.name ?? song.artist)
    }

    private var pendingAutoCount: Int {
        guard entries.indices.contains(queueIndex) else { return 0 }
        return min(Int(queueView?.otherTail ?? 0), entries.count - queueIndex - 1)
    }

    /// The listener's own songs still to come, including the part of the list not in the player yet.
    private var leftAfterCurrent: Int {
        max(0, entries.count - queueIndex - 1 - pendingAutoCount) + Int(queueView?.unfed ?? 0)
    }

    private var autoplayAllowed: Bool { autoplayEnabled && station == nil && repeatMode == .off }

    /// Whether the Autoplay section shows. It does once the list's last song plays, ignoring queued songs.
    var autoplayShown: AutoplayShown {
        _ = queueVersion
        guard currentSong != nil else { return .hidden }
        let repeatCore: RepeatMode = switch repeatMode { case .off: .off; case .all: .all; case .one: .one }
        let listLeft = max(0, leftAfterCurrent - queuedCount)
        let has = pendingAutoCount > 0 || !autoplaySongs.isEmpty
        switch autoplaySection(enabled: autoplayEnabled, repeat: repeatCore, station: station != nil, hasSongs: has, listLeft: UInt64(listLeft), ready: autoplayReady) {
        case .on: return .on
        case .finding: return .finding
        case .repeat: return .repeating
        case .off, .station, .empty, .later: return .hidden
        }
    }

    var autoplayCount: Int {
        _ = queueVersion
        guard autoplayAllowed else { return 0 }
        // While the plan is being found, only count a song already handed to the player.
        return pendingAutoCount + (autoplayReady ? autoplaySongs.count : 0)
    }

    var autoplayBasedOn: String? {
        _ = queueVersion
        if let source = queueSource, source.radio || source.link != nil {
            if case .songs(genre: nil)? = source.link {} else { return source.title }
        }
        guard let id = queue?.anchorSongs(count: 1).last else { return nil }
        let title = entries.first { $0.song.id == id }?.song.title
        return title?.isEmpty == false ? title : nil
    }

    func autoplay(at offset: Int) -> QueueEntry? {
        guard offset >= 0, offset < autoplayCount else { return nil }
        let pending = pendingAutoCount
        if offset < pending {
            let entry = entries[entries.count - pending + offset]
            return QueueEntry(id: entry.id, song: library.collection.apply(entry.song), auto: true)
        }
        let song = autoplaySongs[offset - pending]
        // Negative ids keep these apart from queue entries.
        return QueueEntry(id: -1 - (offset - pending), song: library.collection.apply(song), auto: true)
    }

    func setQueueVisible(_ visible: Bool) {
        queueVisible = visible
        if visible { scheduleAutoplayUpdate() }
    }

    func removeAutoplay(_ offsets: [Int]) {
        let pending = pendingAutoCount
        let upCount = upcomingCount
        var gone: [UInt64] = []
        for offset in Set(offsets) where offset >= 0 && offset < autoplayCount {
            if offset < pending {
                autoplayCore?.remove(song: Self.autoplaySong(entries[entries.count - pending + offset].song))
                // Songs already handed to the player leave its queue too.
                gone.append(UInt64(upCount + offset))
            } else {
                autoplayCore?.remove(song: Self.autoplaySong(autoplaySongs[offset - pending]))
            }
        }
        if !gone.isEmpty { edit { $0.remove(offsets: gone) } }
        refreshAutoplaySongs()
        saveSession()
    }

    /// Plays an Autoplay song right after the current one, keeping Next in Queue. Songs above it leave the plan,
    /// and it stays behind for Previous. It ends a station or following, and the queue is rebuilt from it.
    func playAutoplay(_ offset: Int) {
        guard let entry = autoplay(at: offset), let queue else { return }
        let pending = pendingAutoCount
        if offset < pending {
            edit { $0.jump(index: UInt64(entries.count - pending + offset)) }
            return
        }
        let above = (0..<offset).compactMap { autoplay(at: $0)?.song }
        for song in above { autoplayCore?.remove(song: Self.autoplaySong(song)) }
        if pending > 0 { queue.dropOtherTail() }
        remember([entry.song])
        let held = station != nil || along != nil
        // While something else holds the player, only the model changes. The whole queue is rebuilt below.
        queue.playOther(song: Self.item(entry.song))
        if held {
            if station != nil { station = nil; liveItemID = nil; liveTitle = nil; songBeforeStation = nil }
            dropAlong()
            queue.restartAt(index: queue.view().pos)
        }
        mirror()
        autoplaySongs.removeAll { song in song.id == entry.song.id || above.contains { $0.id == song.id } }
        saveSession()
    }

    /// Moves queue rows, from Up Next or the Autoplay section, to before Up Next position `before`.
    /// Moved songs take the section they land in, and Autoplay songs become the listener's own.
    func moveInQueue(_ rows: [Int], before: Int, queued: Bool) {
        guard let queue, station == nil else { return }
        let upCount = upcomingCount
        let pending = pendingAutoCount
        // Offered songs join the end of the queue first, so every moved song is in the queue before the single move.
        let offered = rows.filter { $0 >= upCount + pending }.compactMap { autoplay(at: $0 - upCount)?.song }
        remember(offered)
        for song in offered { queue.handOver(song: Self.item(song), play: false) }
        autoplaySongs.removeAll { song in offered.contains { $0.id == song.id } }
        var nextOffered = upCount + pending
        let moved = rows.map { row -> UInt64 in
            if row < upCount + pending { return UInt64(row) }
            nextOffered += 1
            return UInt64(nextOffered - 1)
        }
        queue.moveEntries(offsets: moved, before: UInt64(max(0, before)), intoQueued: queued)
        mirror()
        refreshAutoplaySongs()
        saveSession()
    }

    private func scheduleAutoplayUpdate() {
        guard !autoplayScheduled else { return }
        autoplayScheduled = true
        Task { @MainActor [weak self] in
            guard let self else { return }
            self.autoplayScheduled = false
            self.updateAutoplay()
        }
    }

    /// Sends the seeds, takes the server's answer, and decides whether Autoplay takes over now.
    private func updateAutoplay() {
        guard let autoplay = autoplayCore, restored, !entries.isEmpty else { return }
        guard autoplayAllowed else {
            // With repeat, a station, or Autoplay off, anything Autoplay put in the player leaves it.
            if station == nil, pendingAutoCount > 0 { edit { $0.dropOtherTail() } }
            if !autoplaySongs.isEmpty { autoplaySongs = [] }
            return
        }
        // Anchors come from the end of the listener's list, never from Autoplay's songs, so one that plays doesn't
        // start a new context.
        if let queue { autoplay.setContext(key: queue.context(), anchors: queue.anchorSongs(count: Self.anchorCount)) }
        refreshAutoplaySongs()
        if (queueVisible || leftAfterCurrent <= 2), !autoplayAsking, !radioWaiting {
            autoplayAsking = true
            Task { [weak self] in
                let changed = await autoplay.refresh()
                guard let self else { return }
                self.autoplayAsking = false
                if changed { self.updateAutoplay() }
            }
        }
        startAutoplayIfNeeded()
    }

    private func refreshAutoplaySongs() {
        guard let autoplay = autoplayCore, autoplayAllowed else { return }
        let queue = entries.map { Self.autoplaySong($0.song) }
        let songs = autoplay.songs(queue: queue, limit: UInt32(Self.autoplayShownMax)).map { SongRow(core: $0) }
        if songs.map(\.id) != autoplaySongs.map(\.id) { autoplaySongs = songs }
        let ready = autoplay.ready()
        if ready != autoplayReady { autoplayReady = ready }
    }

    /// Puts Autoplay's first song in the player once nothing of the listener's own is left, so the join is gapless.
    private func startAutoplayIfNeeded() {
        guard pendingAutoCount == 0, !radioWaiting, entries.indices.contains(queueIndex), let first = autoplaySongs.first,
              autoplayStartsNow(enabled: autoplayEnabled, repeat: repeatMode == .off ? .off : repeatMode == .all ? .all : .one,
                                station: station != nil, leftAfterCurrent: UInt64(leftAfterCurrent)) else { return }
        // Before the plan is final its songs are only a stand-in. One plays if the context has already ended, but none
        // is lined up early.
        guard autoplayReady || queueEnded else { return }
        let play = queueEnded
        queueEnded = false
        remember([first])
        edit { $0.handOver(song: Self.item(first), play: play) }
        // Looks up its BPM for the flow step, when it has no tag.
        if let autoplay = autoplayCore { Task.detached(priority: .utility) { await autoplay.lookUpBpm(songId: first.id) } }
        refreshAutoplaySongs()
        saveSession()
    }

    // MARK: Now Playing and session

    private func publishNowPlaying() {
        nowPlaying.update(song: currentSong, isPlaying: isPlaying && !isWaitingForServer, position: heardPosition)
        publishDiscord()
    }

    nonisolated private struct SavedSession: Codable, Sendable {
        var ids: [String]
        var index: Int
        var positionMs: UInt64
        /// Read by older builds, which know only this field.
        var repeatOn: Bool
        /// Missing in older sessions.
        var repeatMode: PlayerRepeat?
        var shuffle: Bool
        var queuedRun: Int?
        /// leaves the queue once another song plays.
        var currentQueued: Bool?
        var source: SavedSource?
    }

    /// A `PlaySource` as saved, using the start route's name.
    nonisolated private struct SavedSource: Codable, Sendable {
        var name: String
        var radio: Bool
        var link: String?
    }

    private static func saved(_ s: PlaySource) -> SavedSource {
        let link: String? = switch s.link {
        case .album(let id): "album:\(id)"
        case .playlist(let id): "playlist:\(id)"
        case .artist(let id): "artist:\(id)"
        case .artistTop(let id): "artist-top:\(id)"
        case .mix(let id): "mix:\(id)"
        case .liked: "liked"
        case .songs(let genre): genre.map { "genre:\($0)" } ?? "songs"
        default: nil
        }
        return SavedSource(name: s.name, radio: s.radio, link: link)
    }

    private static func restored(_ s: SavedSource) -> PlaySource {
        let link: Route? = s.link.flatMap { name in
            let parts = name.split(separator: ":", maxSplits: 1).map(String.init)
            let value = parts.count > 1 ? parts[1] : ""
            switch parts[0] {
            case "album": return .album(value)
            case "playlist": return .playlist(value)
            case "artist": return .artist(value)
            case "artist-top": return .artistTop(value)
            case "mix": return .mix(value)
            case "liked": return .liked
            case "genre": return .songs(genre: value)
            case "songs": return .songs(genre: nil)
            default: return nil
            }
        }
        return PlaySource(name: s.name, radio: s.radio, link: link)
    }

    private var sessionURL: URL? { CoreMode.sessionFile }
    /// The queue's model, saved beside the session only when it changed.
    private var queueFileURL: URL? { sessionURL?.deletingLastPathComponent().appendingPathComponent("queue.json") }

    private var savedSession: SavedSession {
        SavedSession(ids: entries.map(\.song.id), index: queueIndex, positionMs: UInt64(max(0, heardPosition) * 1000), repeatOn: repeatMode != .off, repeatMode: repeatMode,
                     shuffle: shuffle, queuedRun: queuedCount, currentQueued: entries[queueIndex].queued, source: queueSource.map(Self.saved))
    }

    private func saveSession() {
        lastSaved = CFAbsoluteTimeGetCurrent()
        guard restored else { return }
        guard entries.indices.contains(queueIndex) else { return }
        let saved = savedSession
        // The server's copy for other devices holds the listener's songs, never Autoplay's.
        if station == nil {
            let own = max(queueIndex + 1, entries.count - pendingAutoCount)
            core?.noteQueue(songIds: entries.prefix(own).map(\.song.id), index: UInt32(queueIndex), positionMs: saved.positionMs)
        }
        guard let url = sessionURL, let data = try? JSONEncoder().encode(saved) else { return }
        let queue = queue, queuePath = queueFileURL?.path
        queueWork.async {
            try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try? data.write(to: url)
            if let queue, let queuePath { _ = queue.save(path: queuePath) }
        }
    }

    /// Restores the last queue, paused where it was. Nothing plays until asked. A session without a saved model
    /// restores its queue as the list.
    private func restoreSession() {
        guard let queue, let core, let url = sessionURL else { restored = true; return }
        let queuePath = queueFileURL?.path
        Task { [weak self] in
            let found = await Task.detached(priority: .utility) { () -> (SavedSession, [SongRow])? in
                guard let data = try? Data(contentsOf: url), let saved = try? JSONDecoder().decode(SavedSession.self, from: data), !saved.ids.isEmpty, saved.index < saved.ids.count else { return nil }
                // Something already started playing, so that queue stays.
                guard queue.view().entries.isEmpty else { return nil }
                if let queuePath, queue.restore(path: queuePath, currentId: saved.ids[saved.index]) {
                    let ids = Array(Set(queue.view().entries.map(\.songId)))
                    return (saved, core.songsById(ids: ids).map { SongRow(core: $0) })
                }
                let rows = core.songsById(ids: saved.ids).map { SongRow(core: $0) }
                guard rows.count == saved.ids.count else { return nil }
                let run = max(0, min(saved.queuedRun ?? 0, rows.count - saved.index - 1))
                let first = (saved.currentQueued ?? false) ? saved.index : saved.index + 1
                queue.replacePlain(songs: rows.map(Self.item), pos: UInt64(saved.index), queuedStart: UInt64(first), queuedEnd: UInt64(saved.index + 1 + run), shuffle: saved.shuffle)
                return (saved, rows)
            }.value
            guard let self else { return }
            self.restored = true
            guard let (saved, rows) = found, self.entries.isEmpty else { return }
            self.remember(rows)
            self.setSource(saved.source.map(Self.restored))
            self.edit { $0.load(startMs: saved.positionMs, paused: true) }
            guard self.entries.indices.contains(self.queueIndex) else { return }
            self.setPosition(Double(saved.positionMs) / 1000)
            self.isPlaying = false
            self.status = "paused"
            self.setCurrent(self.entries[self.queueIndex].song)
            self.publishNowPlaying()
            // The saved model may disagree with the shuffle switch. The queue follows the switch, and a missing mix is
            // worked out now.
            if queue.view().shuffle != self.shuffle {
                self.reshuffle()
            } else if queue.view().mixing {
                let style = self.shuffleStyle
                self.background { queue in _ = queue.mix(style: style) }
            }
        }
    }
}

@MainActor final class NowPlayingBridge {
    private weak var player: CorePlayerModel?
    private var artwork: (songID: String, art: MPMediaItemArtwork)?

    /// The handlers this bridge added, so `detach` removes only its own. The command center is shared by the app,
    /// and `removeTarget(nil)` would also remove a newer player's handlers.
    nonisolated struct Targets: @unchecked Sendable { var list: [(command: MPRemoteCommand, token: Any)] = [] }
    private var targets = Targets()

    func attach(_ player: CorePlayerModel) {
        self.player = player
        if targets.list.isEmpty { targets = registerCommands() }
    }

    func detach() {
        player = nil
        for target in targets.list { target.command.removeTarget(target.token) }
        targets = Targets()
    }

    // nonisolated so these handlers don't inherit the main actor. MediaPlayer calls them on its own queue.
    nonisolated private func registerCommands() -> Targets {
        let center = MPRemoteCommandCenter.shared()
        var out = Targets()
        func add(_ command: MPRemoteCommand, _ handler: @escaping (MPRemoteCommandEvent) -> MPRemoteCommandHandlerStatus) {
            out.list.append((command, command.addTarget(handler: handler)))
        }
        add(center.playCommand) { [weak self] _ in self?.send { $0.playIfPaused() } ?? .noSuchContent }
        add(center.pauseCommand) { [weak self] _ in self?.send { $0.pauseIfPlaying() } ?? .noSuchContent }
        add(center.togglePlayPauseCommand) { [weak self] _ in self?.send { $0.togglePlayPause() } ?? .noSuchContent }
        add(center.nextTrackCommand) { [weak self] _ in self?.send { $0.next() } ?? .noSuchContent }
        add(center.previousTrackCommand) { [weak self] _ in self?.send { $0.previous() } ?? .noSuchContent }
        // Repeat types map one to one. Any shuffle type means on.
        add(center.changeRepeatModeCommand) { [weak self] event in
            guard let event = event as? MPChangeRepeatModeCommandEvent else { return .commandFailed }
            let mode: PlayerRepeat = switch event.repeatType { case .one: .one; case .all: .all; default: .off }
            return self?.send { if $0.station == nil { $0.repeatMode = mode } } ?? .noSuchContent
        }
        add(center.changeShuffleModeCommand) { [weak self] event in
            guard let event = event as? MPChangeShuffleModeCommandEvent else { return .commandFailed }
            let on = event.shuffleType != .off
            return self?.send { if $0.station == nil { $0.shuffle = on } } ?? .noSuchContent
        }
        add(center.changePlaybackPositionCommand) { [weak self] event in
            guard let event = event as? MPChangePlaybackPositionCommandEvent else { return .commandFailed }
            let at = event.positionTime
            return self?.send { $0.seek(to: at) } ?? .noSuchContent
        }
        return out
    }

    nonisolated private func send(_ action: @escaping @Sendable @MainActor (CorePlayerModel) -> Void) -> MPRemoteCommandHandlerStatus {
        Task { @MainActor [weak self] in
            if let player = self?.player { action(player) }
        }
        return .success
    }

    func setModes(repeatMode: PlayerRepeat, shuffle: Bool) {
        let center = MPRemoteCommandCenter.shared()
        center.changeRepeatModeCommand.currentRepeatType = switch repeatMode { case .off: .off; case .all: .all; case .one: .one }
        center.changeShuffleModeCommand.currentShuffleType = shuffle ? .items : .off
    }

    func setArtwork(path: String, songID: String) {
        guard let image = NSImage(contentsOfFile: path) else { return }
        artwork = (songID, Self.makeArtwork(image))
        if let current = MPNowPlayingInfoCenter.default().nowPlayingInfo {
            var info = current
            info[MPMediaItemPropertyArtwork] = artwork?.art
            MPNowPlayingInfoCenter.default().nowPlayingInfo = info
        }
    }

    /// nonisolated so the image request, which arrives on MediaPlayer's own queue, doesn't assert the main actor.
    nonisolated private static func makeArtwork(_ image: NSImage) -> MPMediaItemArtwork {
        let size = image.size
        nonisolated(unsafe) let shared = image
        return MPMediaItemArtwork(boundsSize: size) { @Sendable _ in shared }
    }

    func update(song: SongRow?, isPlaying: Bool, position: Double) {
        let center = MPNowPlayingInfoCenter.default()
        guard let song else {
            center.nowPlayingInfo = nil
            center.playbackState = .stopped
            return
        }
        var info: [String: Any] = [
            MPMediaItemPropertyTitle: song.title,
            MPMediaItemPropertyArtist: song.artist,
            MPMediaItemPropertyAlbumTitle: song.album,
            MPMediaItemPropertyPlaybackDuration: Double(song.duration),
            MPNowPlayingInfoPropertyElapsedPlaybackTime: position,
            MPNowPlayingInfoPropertyPlaybackRate: isPlaying ? 1.0 : 0.0,
            MPNowPlayingInfoPropertyMediaType: MPNowPlayingInfoMediaType.audio.rawValue,
        ]
        if let artwork, artwork.songID == song.id { info[MPMediaItemPropertyArtwork] = artwork.art }
        center.nowPlayingInfo = info
        center.playbackState = isPlaying ? .playing : .paused
    }

    /// Handler counts per shared command across all bridges, read from the commands' descriptions. Diagnostics only.
    static var handlerCounts: [String: Int] {
        let center = MPRemoteCommandCenter.shared()
        let commands: [(String, MPRemoteCommand)] = [("play", center.playCommand), ("pause", center.pauseCommand), ("toggle", center.togglePlayPauseCommand), ("next", center.nextTrackCommand), ("previous", center.previousTrackCommand), ("seek", center.changePlaybackPositionCommand), ("repeat", center.changeRepeatModeCommand), ("shuffleMode", center.changeShuffleModeCommand)]
        var out: [String: Int] = [:]
        for (name, command) in commands {
            guard let range = command.description.range(of: #"handlers=\[[^\]]*\]"#, options: .regularExpression) else { continue }
            let list = command.description[range].dropFirst("handlers=[".count).dropLast()
            out[name] = list.isEmpty ? 0 : list.split(separator: ",").count
        }
        return out
    }

    var snapshot: [String: Any] {
        let center = MPNowPlayingInfoCenter.default()
        let commands = MPRemoteCommandCenter.shared()
        var out: [String: Any] = ["state": center.playbackState.rawValue, "targets": targets.list.count, "handlers": Self.handlerCounts,
                                  "repeatType": commands.changeRepeatModeCommand.currentRepeatType.rawValue, "shuffleType": commands.changeShuffleModeCommand.currentShuffleType.rawValue]
        if let info = center.nowPlayingInfo {
            out["title"] = info[MPMediaItemPropertyTitle] as? String ?? ""
            out["elapsed"] = info[MPNowPlayingInfoPropertyElapsedPlaybackTime] as? Double ?? -1
            out["rate"] = info[MPNowPlayingInfoPropertyPlaybackRate] as? Double ?? -1
        }
        return out
    }
}

extension CorePlayerModel {
    var debugSnapshot: PlayerSnapshot? { player?.snapshot() }
    /// For `-naviaCoreCheck`: the queue as titles, with "> " marking the current song and "[q] " queued songs.
    var debugQueue: (index: Int, rows: [String]) {
        let lower = max(0, queueIndex - 3), upper = min(entries.count, queueIndex + 8)
        let rows = (lower..<upper).map { i in (i == queueIndex ? "> " : "") + (entries[i].queued ? "[q] " : "") + entries[i].song.title }
        return (queueIndex, rows)
    }
    var nowPlayingSnapshot: [String: Any] { nowPlaying.snapshot }
    /// For `-naviaCoreCheck`: whether the queue's model, this mirror and the player agree, plus the model's state.
    func debugVerify() -> [String: Any] {
        guard let view = queue?.view(), let snap = player?.snapshot() else { return ["ok": false, "why": "no queue"] }
        let model = view.entries.map { "\($0.songId)\($0.kind == .queued ? "+q" : "")" }
        let engine = snap.queue.map { "\($0.id)\($0.queued ? "+q" : "")" }
        let mirrored = entries.map { "\($0.song.id)\($0.queued ? "+q" : "")" }
        let ok = model == engine && model == mirrored && snap.index.map(Int.init) == Int(view.pos) && queueIndex == Int(view.pos)
        return ["ok": ok, "count": view.entries.count, "pos": Int(view.pos), "coreIndex": snap.index.map(Int.init) ?? -1, "shuffle": view.shuffle, "mixing": view.mixing,
                "queuedRun": Int(view.queuedRun), "otherTail": Int(view.otherTail), "unfed": Int(view.unfed), "listLeft": Int(view.listLeft),
                "firstMismatch": zip(model, engine).enumerated().first { $0.element.0 != $0.element.1 }?.offset ?? (model.count == engine.count ? -1 : min(model.count, engine.count))]
    }

    #if DEBUG
    /// For `-naviaCoreCheck`: attaches and detaches a second bridge while this one is live, as during an account
    /// switch. This player's handlers must survive it.
    func debugBridgeOverlap() -> [String: Any] {
        let other = NowPlayingBridge()
        other.attach(self)
        let during = NowPlayingBridge.handlerCounts
        other.detach()
        return ["during": during, "after": NowPlayingBridge.handlerCounts]
    }
    #endif
}

nonisolated enum PlaybackHelpers {
    static func systemOutputs() -> [(id: String, name: String)] {
        ((try? outputDevices()) ?? []).map { (id: $0.id, name: $0.name) }
    }
}

extension LyricLine {
    /// How much of the line is sung at `ms`, as a fraction of its characters, from the server's word timing.
    /// Nil without timing, in which case the fill is estimated from the line's span.
    nonisolated func sungFraction(atMs ms: Int) -> Double? {
        guard !words.isEmpty else { return nil }
        let row = LyricLineRow(startMs: start.map { UInt32(max(0, $0)) }, text: text, endMs: endMs.map { UInt32(max(0, $0)) },
                               words: words.map { LyricWordRow(start: $0.start, end: $0.end, startMs: $0.startMs, endMs: $0.endMs) })
        return lyricSungFraction(line: row, ms: UInt32(max(0, ms))).map(Double.init)
    }
}
