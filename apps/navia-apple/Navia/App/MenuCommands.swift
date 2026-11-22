import AppKit
import SwiftUI

/// each item calls what the app's own buttons call.
struct NaviaCommands: Commands {
    let shell: ShellModel
    private var player: any PlayerViewModel { shell.player }

    var body: some Commands {
        // it opens Settings, where About shows the answer.
        CommandGroup(after: .appInfo) {
            Button(L("Check for Updates…")) {
                shell.openSettings()
                AppUpdates.shared.check()
            }
        }
        CommandGroup(replacing: .appSettings) {
            Button(L("Settings…")) { shell.openSettings() }.keyboardShortcut(",", modifiers: .command)
        }
        CommandGroup(after: .newItem) {
            Button(L("Get Info")) { shell.infoShortcut() }.keyboardShortcut("i", modifiers: .command)
        }
        // its keys are caught before the menus (ShellModel), so AppKit's Minimize All never takes them.
        CommandGroup(after: .windowArrangement) {
            Divider()
            Toggle(L("Mini Player"), isOn: Binding(get: { MiniPlayer.shared.isOpen }, set: { _ in MiniPlayer.shared.toggle(shell: shell) }))
                .keyboardShortcut("m", modifiers: [.command, .option])
        }
        CommandGroup(before: .sidebar) {
            Button(shell.sidebarShown ? L("Hide Sidebar") : L("Show Sidebar")) { shell.toggleSidebar() }
                .keyboardShortcut("s", modifiers: [.command, .control])
                .disabled(shell.connect != nil)
            Divider()
            let lyrics = player.mayHaveLyrics && player.station == nil
            Toggle(L("Lyrics"), isOn: Binding(get: { shell.playerViewShown && shell.shownPlayerView == .lyrics }, set: { _ in shell.togglePlayerView(.lyrics) }))
                .disabled(player.current == nil || !lyrics)
            Toggle(L("Queue"), isOn: Binding(get: { shell.playerViewShown && shell.shownPlayerView == .queue }, set: { _ in shell.togglePlayerView(.queue) }))
                .disabled(player.current == nil)
            Toggle(L("Full Screen Player"), isOn: Binding(get: { shell.expanded }, set: { shell.setExpanded($0) }))
                .keyboardShortcut("f", modifiers: [.command, .shift])
                .disabled(player.current == nil)
            Divider()
        }
        CommandMenu(L("Playback")) {
            let none = player.current == nil
            let station = player.station != nil
            Button(player.isPlaying ? L("Pause") : L("Play")) { player.togglePlayPause() }
                .keyboardShortcut(.space, modifiers: [])
                .disabled(none)
            Button(L("Next")) { player.next() }
                .keyboardShortcut(.rightArrow, modifiers: .command)
                .disabled(none || station)
            Button(L("Previous")) { player.previous() }
                .keyboardShortcut(.leftArrow, modifiers: .command)
                .disabled(none || station)
            Divider()
            Toggle(L("Shuffle"), isOn: Binding(get: { player.shuffle }, set: { player.shuffle = $0 }))
                .disabled(station)
            Picker(L("Repeat"), selection: Binding(get: { player.repeatMode }, set: { player.repeatMode = $0 })) {
                Text(L("Off")).tag(PlayerRepeat.off)
                Text(L("All||repeat")).tag(PlayerRepeat.all)
                Text(L("One||repeat")).tag(PlayerRepeat.one)
            }
            .disabled(station)
            Divider()
            Button(L("Increase Volume")) { player.stepVolume(1) }
                .keyboardShortcut(.upArrow, modifiers: .command)
            Button(L("Decrease Volume")) { player.stepVolume(-1) }
                .keyboardShortcut(.downArrow, modifiers: .command)
        }
        CommandMenu(L("Go")) {
            Button(L("Back")) { shell.goBack() }
                .keyboardShortcut("[", modifiers: .command)
                .disabled(!shell.canGoBack)
            Button(L("Forward")) { shell.goForward() }
                .keyboardShortcut("]", modifiers: .command)
                .disabled(!shell.canGoForward)
            Divider()
            ForEach(SidebarPages.top + SidebarPages.library, id: \.1) { page in
                let button = Button(page.1) { open(page.0) }
                if page.0 == .browse() { button.keyboardShortcut("f", modifiers: .command) } else { button }
            }
            Button(L("All Playlists")) { open(.playlists) }
        }
    }

    /// over the full screen player too.
    private func open(_ route: Route) {
        guard shell.connect == nil else { return }
        shell.setExpanded(false)
        shell.go(route)
    }

    /// inside a text field or on a focused button these keys go to it, not the menu.
    static func passesToFocus(_ event: NSEvent) -> Bool {
        let mods = event.modifierFlags.intersection([.command, .control, .option, .shift])
        let space = event.keyCode == 49 && mods.isEmpty
        let arrows: Set<UInt16> = [123, 124, 125, 126]
        return space || (mods == .command && arrows.contains(event.keyCode))
    }
}
