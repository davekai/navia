import SwiftUI
import NaviaCoreFFI

private let licenseURL = URL(string: "https://github.com/davekai/navia/blob/main/LICENSE")!

/// the Equalizer page's own settings, so the search finds them on its row.
private let equalizerTerms = [N_("Equalizer mode"), N_("Graphic"), N_("Parametric"), N_("Preset"), N_("Preamp"), N_("Custom filters"), N_("Reset to Flat"), N_("Save as preset"), N_("Balance"), N_("Mono"), N_("Crossfeed"), N_("Crossfeed strength"), N_("Soft limiter"), N_("Headphone Correction"), N_("Headphone model")]

/// rows without a backend on the Mac are disabled and say why.
struct SettingsPage: View {
    let shell: ShellModel
    @Environment(\.pageContext) private var ctx
    @State private var openAtLogin = OpenAtLogin()
    @State private var query = ""

    private var theme: Theme { shell.theme }

    var body: some View {
        PageFrame(shell: shell, nav: .none, title: L("Settings"), hasRight: true) {
            SearchPill(placeholder: L("Search Settings"), text: $query, accent: theme.accent, width: pillSearchWidth(ctx))
        } content: {
            PageScroll { content }
        }
        .onAppear {
            shell.loadOutputs()
            openAtLogin.refresh()
        }
    }

    /// sections the search leaves empty resolve to nothing, so none left means nothing matched.
    private var content: some View {
        Group(subviews: sections) { found in
            if found.isEmpty {
                EmptyStateView(symbol: "magnifyingglass", title: L("No settings match"), hint: L("Nothing matches “{query}”.", ["query": query.trimmingCharacters(in: .whitespaces)]),
                               compact: true, actionTitle: L("Clear Search")) { query = "" }
            } else {
                VStack(alignment: .leading, spacing: T.settingsSectionGap) { found }
            }
        }
        .environment(\.settingsQuery, settingsSearchWords(query))
        .padding(.horizontal, T.gutter)
        .frame(maxWidth: T.settingsMaxW)
        .frame(maxWidth: .infinity)
    }

    @ViewBuilder private var sections: some View {
        playback
        appearance
        player
        startup
        library
        StorageSection(shell: shell)
        connections
        if let listen = shell.player.listen { SpotifyFriendsSection(listen: listen, theme: theme) }
        about
    }

    // MARK: Playback

    private var playback: some View {
        SettingsSection(title: L("Playback")) {
            SettingsRow(label: L("Between songs"), hint: L("Gapless keeps albums seamless. Smart crossfades between unrelated songs, and stays gapless within an album. Mix shapes each transition to the music.")) {
                SegmentedChoice(look: .settings, value: shell.betweenSongs,
                                options: [.init(value: .gapless, label: L("Gapless")), .init(value: .crossfade, label: L("Crossfade")),
                                          .init(value: .smart, label: L("Smart")), .init(value: .mix, label: L("Mix"))],
                                accent: theme.accent, label: L("Between songs")) { shell.betweenSongs = $0 }
            }
            // Mix chooses each transition's length itself.
            if shell.betweenSongs == .crossfade || shell.betweenSongs == .smart {
                SettingsRow(label: L("Crossfade length"), hint: Ln("{count} second", "{count} seconds", shell.crossfadeSeconds)) {
                    PlainSlider(label: L("Crossfade length"), value: Double(shell.crossfadeSeconds), min: 1, max: 12, look: .native(accent: theme.accent), valueText: Ln("{count} second", "{count} seconds", shell.crossfadeSeconds),
                                onChange: { shell.crossfadeSeconds = Int($0) })
                }
            }
            SettingsRow(label: L("Shuffle"), hint: shell.player.shuffleStyle == .fewerRepeats ? L("Fewer Repeats plays songs you heard recently later.") : L("Standard plays the list in a plain random order.")) {
                SegmentedChoice(look: .settings, value: shell.player.shuffleStyle,
                                options: [.init(value: .fewerRepeats, label: L("Fewer Repeats")), .init(value: .standard, label: L("Standard"))],
                                accent: theme.accent, label: L("Shuffle")) { shell.player.shuffleStyle = $0 }
            }
            SettingsRow(label: L("Match the song's sample rate"), hint: L("Switches your output device to each song's sample rate instead of converting the song.")) {
                ToggleSwitch(isOn: Binding(get: { shell.player.matchSampleRate }, set: { shell.player.matchSampleRate = $0 }), accent: theme.accent, label: L("Match the song's sample rate"))
            }
            SettingsRow(label: L("Autoplay"), hint: L("When your music ends, keep playing similar songs. Starts only once nothing is left, so after every song of a shuffled list, and never while Repeat is on.")) {
                ToggleSwitch(isOn: Binding(get: { shell.player.autoplayEnabled }, set: { shell.player.autoplayEnabled = $0 }), accent: theme.accent, label: L("Autoplay"))
            }
            if shell.player.autoplayEnabled {
                SettingsRow(label: L("Autoplay mode"), hint: shell.player.autoplayMode == .navia ? L(autoplayNaviaHint) : L(autoplayServerHint)) {
                    SegmentedChoice(look: .settings, value: shell.player.autoplayMode,
                                    options: [.init(value: .navia, label: L("Navia")), .init(value: .server, label: L("Server"))],
                                    accent: theme.accent, label: L("Autoplay mode")) { shell.player.autoplayMode = $0 }
                }
            }
            // The Deezer switch is never dimmed.
            SettingsRow(label: L("Use Deezer for recommendations"),
                        hint: L("Better Autoplay and Artist Radio, and a Heat column on albums. Sends artist and song names to Deezer and ListenBrainz from this computer. No account needed.")) {
                ToggleSwitch(isOn: Binding(get: { shell.deezerChoice == true }, set: { shell.setDeezerChoice($0) }), accent: theme.accent, label: L("Use Deezer for recommendations"))
            }
            VolumeLevelingRow(shell: shell)
            StreamingQualityRow(shell: shell)
            MeteredQualityRow(shell: shell)
            SettingsLinkRow(label: L("Equalizer"), hint: shell.equalizer.summary(outputLabel: shell.outputLabel(shell.equalizer.state.output)), terms: equalizerTerms.map { L($0) }) {
                shell.go(.equalizer)
            }
        }
    }

    // MARK: Appearance

    private var appearance: some View {
        SettingsSection(title: L("Appearance")) {
            SettingsRow(label: L("Accent color"), hint: theme.followsSystem ? L("Matches your Mac's accent color.") : nil) { AccentPicker(theme: theme) }
            SettingsRow(label: L("Window translucency"), hint: translucencyHint, dim: shell.systemReduceTransparency) {
                ToggleSwitch(isOn: Binding(get: { shell.windowTranslucency && !shell.systemReduceTransparency },
                                           set: { if !shell.systemReduceTransparency { shell.windowTranslucency = $0 } }),
                             accent: theme.accent, label: L("Window translucency"))
                    .opacity(shell.systemReduceTransparency ? T.disabledOpacity : 1)
                    .disabled(shell.systemReduceTransparency)
                    #if DEBUG
                    .onGeometryChange(for: CGRect.self, of: { $0.frame(in: .global) }) { DiagnosticsRunner.translucencySwitchFrame = $0 }
                    #endif
            }
            SettingsRow(label: L("Language"), hint: L("Follows your system's language unless you pick one.")) {
                GlassSelect(shell: shell, label: L("Language"), value: shell.language,
                            options: [SelectOption(value: "system", label: L("System"))] + Language.names.map { SelectOption(value: $0.code, label: $0.name) },
                            width: T.px(240)) { shell.language = $0 }
            }
        }
    }

    private var translucencyHint: String {
        shell.systemReduceTransparency ? L("Off while Reduce transparency is on in System Settings.") : L("Shows your desktop blurred behind Navia.")
    }

    // MARK: Player

    private var player: some View {
        SettingsSection(title: L("Player")) {
            SettingsRow(label: L("Interface size"), hint: L("{percent}%. Command and plus or minus also changes it, and Command-0 resets it.", ["percent": String(shell.interfaceSize)])) {
                PlainSlider(label: L("Interface size"), value: Double(shell.interfaceSize), min: 70, max: 130, step: 5, look: .native(accent: theme.accent), valueText: "\(shell.interfaceSize)%",
                            onChange: { shell.setInterfaceSize(Int($0)) })
            }
            SettingsRow(label: L("Position"), hint: L("On narrow windows the dock becomes the bottom bar until there's room.")) {
                SegmentedChoice(look: .settings, value: shell.playerMode, options: [.init(value: .dock, label: L("Right Dock")), .init(value: .bar, label: L("Bottom Bar"))],
                                accent: theme.accent, label: L("Player position")) { shell.playerMode = $0 }
            }
            SettingsRow(label: L("Lyric effects"), hint: L("Glow, rising words and moving lines. Off shows plain synced lyrics.")) {
                ToggleSwitch(isOn: Binding(get: { shell.lyricEffects }, set: { shell.lyricEffects = $0 }), accent: theme.accent, label: L("Lyric effects"))
            }
            SettingsRow(label: L("Lyric sweep"), hint: L("Lights up the lyrics as they're sung. Off highlights the whole line.")) {
                ToggleSwitch(isOn: Binding(get: { shell.lyricSweep }, set: { shell.lyricSweep = $0 }), accent: theme.accent, label: L("Lyric sweep"))
            }
            SettingsRow(label: L("Backing vocals on their own line"), hint: L("Moves words in parentheses to a smaller line under the lyric.")) {
                ToggleSwitch(isOn: Binding(get: { shell.backingVocalsLine }, set: { shell.backingVocalsLine = $0 }), accent: theme.accent, label: L("Backing vocals on their own line"))
            }
            SettingsRow(label: L("Blur lyrics by distance"), hint: nil) {
                ToggleSwitch(isOn: Binding(get: { shell.lyricBlur }, set: { shell.lyricBlur = $0 }), accent: theme.accent, label: L("Blur lyrics"))
            }
        }
    }

    // MARK: Startup

    /// Open at login and the mini player on minimize. Closing the window keeps the macOS behavior.
    private var startup: some View {
        SettingsSection(title: L("Startup")) {
            SettingsRow(label: L("Show the mini player when Navia is minimized"), hint: nil) {
                ToggleSwitch(isOn: Binding(get: { shell.miniOnMinimize }, set: { shell.miniOnMinimize = $0 }), accent: theme.accent, label: L("Show the mini player when Navia is minimized"))
            }
            SettingsRow(label: L("Open at login"), hint: openAtLogin.needsApproval ? L("Allow Navia in System Settings, under General > Login Items.") : nil) {
                ToggleSwitch(isOn: Binding(get: { openAtLogin.isOn }, set: { openAtLogin.set($0) }), accent: theme.accent, label: L("Open at login"))
            }
        }
    }

    // MARK: Library

    private var library: some View {
        SettingsSection(title: L("Library")) {
            SettingsRow(label: L("Server"), hint: serverHint) {
                HStack(spacing: T.px(10)) {
                    ActionButton(title: L("Change Server"), compact: true, enabled: canChangeServer) { shell.openChangeServer() }
                    ActionButton(title: L("Sign Out"), compact: true, enabled: canChangeServer) { shell.requestSignOut() }
                }
                .fixedSize()
            }
            SettingsRow(label: L("Sync library in the background"), hint: L("With this off, Navia only syncs when it starts.")) {
                ToggleSwitch(isOn: Binding(get: { shell.backgroundSync }, set: { shell.backgroundSync = $0 }), accent: theme.accent, label: L("Sync library"))
            }
            DownloadQualityRow(shell: shell)
            // Synced downloads wait on a metered connection unless this is on.
            if let synced = shell.library.synced {
                SettingsRow(label: L("Download on metered connections"),
                            hint: L("Playlists and albums you keep downloaded wait while your system marks the connection as metered, like a phone hotspot. Turn this on to let them download there too. Downloading something yourself always runs.")) {
                    ToggleSwitch(isOn: Binding(get: { synced.downloadOnMetered }, set: { synced.downloadOnMetered = $0 }), accent: theme.accent, label: L("Download on metered connections"))
                }
            }
            shareLinksRow
        }
    }

    /// Share Links: how many, or why not when sharing is off on the server.
    @ViewBuilder private var shareLinksRow: some View {
        if let links = shell.library.shareLinks, links.sharingOn {
            let count = links.links?.count ?? 0
            SettingsLinkRow(label: L("Share Links"), hint: count > 0 ? Ln("{count} link", "{count} links", count) : L("Links to songs, albums and playlists that anyone can open.")) {
                shell.go(.shares)
            }
            .task { if links.links == nil { links.refresh() } }
        } else {
            SettingsRow(label: L("Share Links"), hint: L("Sharing is turned off on this server"), dim: true) { EmptyView() }
        }
    }

    private var discordHint: String {
        shell.player.privateMode ? L("Show what you're listening to on your profile. Hidden while Private mode is on.") : L("Show what you're listening to on your profile.")
    }

    private var scrobbleLocked: Bool { CoreLogin.reportOverride || CoreMode.useSample || shell.activeLogin == nil }

    private var scrobbleHint: String {
        if CoreLogin.reportOverride { return "On because NAVIA_REPORT_PLAYS=1 is set." }
        if CoreMode.useSample { return "Off with sample data." }
        if shell.player.privateMode { return L("Sends plays to the server, which forwards them to whichever services it has set up. Paused while Private mode is on.") }
        return shell.scrobblePlays ? L("Sends plays to the server, which forwards them to whichever services it has set up. Favorites also reach the server while this is on.") : L("Sends plays to the server, which forwards them to whichever services it has set up. While it's off, favorites stay on this device too.")
    }

    private var canChangeServer: Bool { shell.activeLogin?.isDeveloperOverride == false }

    private var serverHint: String {
        guard let login = shell.activeLogin else { return CoreMode.useSample ? "Sample data, for testing." : L("Not signed in.") }
        let values = ["host": login.host, "user": login.user]
        guard login.isDeveloperOverride else { return L("{host}, signed in as {user}.", values) }
        return L("{host}, signed in as {user}. Set by the environment, so it can't be changed here.", values)
    }

    private let autoplayNaviaHint = N_("Picks from the server's suggestions, your plays and favorites, and Deezer and ListenBrainz when they're on.")
    private let autoplayServerHint = N_("The server's similar songs, as they come.")

    // MARK: Connections

    /// Settings > Share what I'm playing and Let listeners control playback hints.
    private let controlHint = N_("People listening along with you can pause, skip, seek and change the song.")
    private let shareHint = N_("Others using Navia on this server can see what you play and listen along, and you can see who else shares.")

    private var connections: some View {
        return SettingsSection(title: L("Connections")) {
            SettingsRow(label: L("Private Mode"), hint: L("Plays aren't counted or sent to scrobbling, others on the server can't see what you're playing, and Discord status is hidden.")) {
                ToggleSwitch(isOn: Binding(get: { shell.player.privateMode }, set: { shell.player.privateMode = $0 }), accent: theme.accent, label: L("Private Mode"))
            }
            // Opt-in, and dimmed in Private mode.
            SettingsRow(label: L("Share what I'm playing"), hint: shell.player.privateMode ? L("Paused while Private mode is on.") : L(shareHint), dim: shell.player.privateMode) {
                ToggleSwitch(isOn: Binding(get: { shell.player.shareListening }, set: { shell.player.shareListening = $0 }), accent: theme.accent, label: L("Share what I'm playing"))
                    .disabled(shell.player.privateMode)
            }
            // Only while sharing and not private.
            if shell.player.shareListening, !shell.player.privateMode, let listen = shell.player.listen {
                SettingsRow(label: L("Let listeners control playback"), hint: L(controlHint)) {
                    ToggleSwitch(isOn: Binding(get: { listen.listenersControl }, set: { listen.listenersControl = $0 }), accent: theme.accent, label: L("Let listeners control playback"))
                }
            }
            SettingsRow(label: L("Discord status"), hint: discordHint) {
                ToggleSwitch(isOn: Binding(get: { shell.discordStatus }, set: { shell.discordStatus = $0 }), accent: theme.accent, label: L("Discord status"))
            }
            SettingsRow(label: L("Discord album art"), hint: L("Can't be changed yet. Which source to use is still undecided."), dim: true) {
                DisabledPill(value: L("Match public covers"))
            }
            SettingsRow(label: L("Covers from"), hint: L("If covers show up blank in Discord, choose External sources.")) {
                GlassSelect(shell: shell, label: L("Covers from"), value: shell.discordCoversFrom,
                            options: [SelectOption(value: "auto", label: L("Automatic")), SelectOption(value: "server", label: L("My server")), SelectOption(value: "search", label: L("External sources"))],
                            width: T.px(240)) { shell.discordCoversFrom = $0 }
            }
            DiscordAddressRow(shell: shell)
            SettingsRow(label: L("Scrobble plays"), hint: scrobbleHint, dim: scrobbleLocked) {
                ToggleSwitch(isOn: Binding(get: { shell.scrobblePlays || CoreLogin.reportOverride }, set: { shell.chooseScrobble($0) }), accent: theme.accent, label: L("Scrobble plays"))
                    .disabled(scrobbleLocked)
                    .opacity(scrobbleLocked ? T.disabledOpacity : 1)
            }
            playThresholdRows(shell.library.playThreshold)
        }
    }

    // MARK: About

    /// The version, Check for Updates and "Check for updates automatically".
    private var about: some View {
        let updates = AppUpdates.shared
        return SettingsSection(title: L("About")) {
            SettingsRow(label: L("Version"), hint: updates.note) {
                HStack(spacing: T.px(10)) {
                    Text(AppUpdates.versionLabel).font(.navia(T.textSM)).foregroundStyle(T.t2.color)
                    ActionButton(title: updates.buttonTitle, compact: true, enabled: updates.status != .checking) { updates.check() }
                }
            }
            SettingsRow(label: L("Check for updates automatically"), hint: nil) {
                ToggleSwitch(isOn: Binding(get: { updates.checkAutomatically }, set: { updates.checkAutomatically = $0 }), accent: theme.accent, label: L("Check for updates automatically"))
            }
            SettingsRow(label: L("License"), hint: L("Licensed under GPL-3.0-or-later.")) {
                ActionButton(title: L("View License"), compact: true) { NSWorkspace.shared.open(licenseURL) }
            }
        }
    }

    /// Why "Count a play after" and "Or after 4 minutes" are dimmed, if they are. They follow the Scrobble plays
    /// setting, not a developer override.
    private func playThresholdReason(_ settings: PlayThresholdSettings?) -> String? {
        if settings == nil { return L("Needs a connection to the server.") }
        if shell.player.privateMode { return L("Paused while Private mode is on.") }
        if !shell.scrobblePlays { return L("Used while Scrobble plays is on.") }
        return nil
    }

    @ViewBuilder private func playThresholdRows(_ settings: PlayThresholdSettings?) -> some View {
        let reason = playThresholdReason(settings)
        let percent = settings?.current.percent ?? 60
        let cap = settings?.current.capAtFourMinutes ?? true
        SettingsRow(label: L("Count a play after"), hint: reason ?? L("Only the time you actually hear counts, and songs under 30 seconds never count."), dim: reason != nil) {
            if let settings, reason == nil {
                GlassSelect(shell: shell, label: L("Count a play after"), value: String(percent),
                            options: settings.choices.map { SelectOption(value: String($0), label: PlayThresholdSettings.percentLabel($0)) }, width: T.px(240)) { value in
                    if let p = UInt32(value) { settings.setPercent(p) }
                }
            } else {
                DisabledPill(value: PlayThresholdSettings.percentLabel(percent))
            }
        }
        SettingsRow(label: L("Or after 4 minutes"), hint: reason ?? L("Long songs also count once you've heard 4 minutes of them."), dim: reason != nil) {
            ToggleSwitch(isOn: Binding(get: { cap }, set: { settings?.setCap($0) }), accent: theme.accent, label: L("Or after 4 minutes"))
                .disabled(reason != nil)
                .opacity(reason != nil ? T.disabledOpacity : 1)
        }
    }
}

/// Settings > Discord "Public server address". While empty and not being edited, it shows whether the server's own
/// address is used, with a button that opens the field. Hidden for external sources.
private struct DiscordAddressRow: View {
    let shell: ShellModel
    @State private var open = false

    private var theme: Theme { shell.theme }

    private var fieldHint: String {
        switch checkPublicAddress(text: shell.discordPublicAddress) {
        case .notHttps: L("Only https addresses can be used.")
        case .invalid: L("Enter the address as https://music.example.org.")
        case .empty, .usable: L("If your server is reachable from the internet, Discord can show its own covers.")
        }
    }

    private var status: String? {
        let using = L("Using your server's address for covers.")
        if shell.discordCoversFrom == "server" {
            let https = shell.activeLogin?.url.lowercased().hasPrefix("https://") ?? false
            return https ? using : L("Only https addresses can be used.")
        }
        guard let known = shell.serverAddressPublic?.isPublic else { return nil }
        return known ? using : L("Your server's address isn't public, so covers come from external sources.")
    }

    var body: some View {
        if shell.discordCoversFrom != "search" {
            if open || !shell.discordPublicAddress.trimmingCharacters(in: .whitespaces).isEmpty {
                SettingsRow(label: L("Public server address"), hint: fieldHint) {
                    PillTextField(placeholder: "https://music.example.org", text: Binding(get: { shell.discordPublicAddress }, set: { shell.discordPublicAddress = $0 }),
                                  accent: theme.accent, width: T.px(240), onSubmit: { shell.applyDiscordPublicAddress() },
                                  onBlur: { shell.applyDiscordPublicAddress(); open = false }, autoFocus: open)
                }
            } else {
                SettingsRow(label: L("Public server address"), hint: status) {
                    TextPillButton(symbol: nil, title: L("Use a different address")) { open = true }
                }
                .onAppear { shell.checkServerAddressPublic() }
                .onChange(of: shell.activeLogin?.url) { shell.checkServerAddressPublic() }
            }
        }
    }
}

/// Settings > Spotify Friends: the friends, adding one by Discord id, and the Lanyard address. Nothing contacts
/// Lanyard until a friend is added.
private struct SpotifyFriendsSection: View {
    let listen: ListenAlongModel
    let theme: Theme
    @State private var draftID = ""
    @State private var draftName = ""

    /// The section's hint and the id field's hint.
    private let friendsHint = N_("Follow friends who listen on Spotify and hear the same songs from your library. For their Spotify activity to be readable, they need to join Lanyard’s Discord server.")
    private let idHint = N_("Their Discord user ID. In Discord, turn on Developer Mode, then right-click them and choose Copy User ID.")

    private func status(_ id: String) -> String {
        guard let f = listen.friends.first(where: { $0.id == id }) else { return L("Checking…") }
        switch f.reach {
        case .waiting: return L("Checking…")
        case .notMonitored: return L("Lanyard can’t see them yet. They need to join Lanyard’s Discord server.")
        case .unreachable: return L("Can’t reach Lanyard right now.")
        case .readable:
            guard f.listening else { return L("Not listening on Spotify right now.") }
            let vars = ["song": f.title, "artist": f.artist]
            return f.song != nil ? L("Listening to “{song}” by {artist}.", vars) : L("Listening to “{song}” by {artist}. Not in your library.", vars)
        }
    }

    private func name(_ saved: SpotifyFriend) -> String {
        listen.friends.first(where: { $0.id == saved.id })?.name ?? saved.name ?? saved.id
    }

    var body: some View {
        let parsed = lanyardFriendId(text: draftID)
        let taken = parsed.map { id in listen.savedFriends.contains(where: { $0.id == id }) } ?? false
        let addHint = !draftID.isEmpty && parsed == nil ? L("A Discord user ID is a number of 17 to 20 digits.") : taken ? L("This friend is already added.") : L(idHint)
        let addressOK = lanyardAddress(text: listen.lanyardAddress) != nil
        SettingsSection(title: L("Spotify Friends")) {
            SettingsRow(label: L("Spotify friends"), hint: L(friendsHint)) {
                TextPillButton(symbol: nil, title: L("About Lanyard")) {
                    if let url = URL(string: lanyardProjectUrl()) { NSWorkspace.shared.open(url) }
                }
            }
            ForEach(listen.savedFriends, id: \.id) { saved in
                SettingsRow(label: name(saved), hint: status(saved.id)) {
                    TextPillButton(symbol: nil, title: L("Remove")) { listen.removeFriend(id: saved.id) }
                }
            }
            SettingsRow(label: L("Add a friend"), hint: addHint) {
                HStack(spacing: T.px(8)) {
                    PillTextField(placeholder: L("Discord user ID"), text: $draftID, accent: theme.accent, width: T.px(200), onSubmit: add)
                    PillTextField(placeholder: L("Name (optional)"), text: $draftName, accent: theme.accent, width: T.px(180), onSubmit: add)
                    TextPillButton(symbol: "plus", title: L("Add"), disabled: parsed == nil || taken, action: add)
                }
            }
            SettingsRow(label: L("Lanyard address"), hint: addressOK ? L("Lanyard’s public service, or the address of your own.") : L("Enter the address as https://lanyard.example.org.")) {
                PillTextField(placeholder: lanyardPublicAddress(), text: Binding(get: { listen.lanyardAddress }, set: { listen.lanyardAddress = $0 }),
                              accent: theme.accent, width: T.px(240),
                              onSubmit: { listen.applyAddress() }, onBlur: { listen.applyAddress() })
            }
        }
    }

    private func add() {
        if listen.addFriend(id: draftID, name: draftName) {
            draftID = ""
            draftName = ""
        }
    }
}
