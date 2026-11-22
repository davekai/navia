import SwiftUI
import NaviaCoreFFI

/// Settings > Storage, with the song cache's limit, each cache's size, Downloads and Clear Cache.
struct StorageSection: View {
    let shell: ShellModel

    var body: some View {
        SettingsSection(title: L("Storage")) {
            if let storage = shell.library.storage {
                rows(storage)
            } else {
                SettingsRow(label: L("Song cache"), hint: L("Needs a connection to the server."), dim: true) { DisabledPill(value: StorageSettings.limitLabel(2_000_000_000)) }
                downloads(bytes: nil)
                SettingsRow(label: L("Clear cache"), hint: L("Removes cached songs, artwork and lyrics. Downloads and your library stay."), dim: true) {
                    ActionButton(title: L("Clear Cache"), compact: true, enabled: false) {}
                }
            }
        }
    }

    @ViewBuilder private func rows(_ storage: StorageSettings) -> some View {
        let usage = storage.usage
        SettingsRow(label: L("Song cache"), hint: L("Songs you play are saved on this device as they stream, so they start at once and play offline. When it's full, the least recently played go first.")) {
            GlassSelect(shell: shell, label: L("Song cache"), value: String(storage.limit), options: storage.choices.map { SelectOption(value: String($0), label: StorageSettings.limitLabel($0)) }, width: T.px(240)) { value in
                if let bytes = UInt64(value) { storage.setLimit(bytes) }
            }
        }
        .onAppear { storage.refresh() }
        SettingsLinkRow(label: L("Cached songs"), hint: cachedHint(storage, usage) ?? "", value: cachedValue(storage, usage)) { shell.go(.cachedSongs) }
        SettingsRow(label: L("Artwork"), hint: nil) { value(usage.map { StorageSettings.sizeLabel($0.artworkBytes) } ?? "") }
        SettingsRow(label: L("Lyrics"), hint: nil) { value(usage.map { StorageSettings.sizeLabel($0.lyricsBytes) } ?? "") }
        downloads(bytes: usage?.downloadsBytes)
        SettingsRow(label: L("Clear cache"), hint: L("Removes cached songs, artwork and lyrics. Downloads and your library stay.")) {
            ActionButton(title: L("Clear Cache"), compact: true) {
                shell.confirm(ConfirmRequest(title: L("Clear the cache?"), message: L("Cached songs, artwork and lyrics are removed from this device. Downloads and your library stay, and anything removed comes from the server again when it's needed."),
                                             confirmTitle: L("Clear Cache"), compact: true) { storage.clear() })
            }
            .fixedSize()
        }
    }

    private func value(_ text: String) -> some View {
        Text(text).font(.navia(T.textMD)).foregroundStyle(T.t2.color).fixedSize()
    }

    private func cachedValue(_ storage: StorageSettings, _ usage: StorageUsage?) -> String {
        guard storage.limit > 0 else { return L("Off") }
        guard let usage else { return "" }
        return L("{used} of {limit}", ["used": StorageSettings.sizeLabel(usage.songCacheBytes), "limit": StorageSettings.limitLabel(storage.limit)])
    }

    private func cachedHint(_ storage: StorageSettings, _ usage: StorageUsage?) -> String? {
        guard storage.limit > 0 else { return L("Nothing is kept while the song cache is off.") }
        guard let usage else { return nil }
        let count = Ln("{count} song", "{count} songs", Int(usage.cachedSongs))
        return usage.songCacheLowDisk ? "\(count). \(L("Paused: the disk is nearly full."))" : count
    }

    private func downloads(bytes: UInt64?) -> some View {
        SettingsLinkRow(label: L("Downloads"), hint: Ln("{count} song on this device", "{count} songs on this device", shell.library.downloadedCount), value: bytes.map(StorageSettings.sizeLabel)) {
            shell.go(.downloads)
        }
    }
}
