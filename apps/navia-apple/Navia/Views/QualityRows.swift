import SwiftUI
import NaviaCoreFFI

private var streamHint: String { L("Original plays the files as they are, bit for bit. Other formats make the server convert each song, using its CPU, and aren't bit-exact. MP3 and AAC can leave up to about 25 ms between gapless tracks; Opus doesn't. If a format isn't available, Navia plays the original.") }
private var downloadHint: String { L("Original keeps the files as they are. Other formats make the server convert each song once, using its CPU. Songs you've already downloaded stay as they are.") }
private var levelingHint: String { L("Uses ReplayGain from the files, limited by their peak so it can't clip. Off plays every file unchanged. On a transcoded stream the peak is the original's, so levels can overshoot slightly.") }

/// A format picker and a maximum bitrate picker. Original has no bitrate, so that side is disabled.
struct QualityRow: View {
    let shell: ShellModel
    let label: String
    let hint: String
    let slot: QualitySettings.Slot

    var body: some View {
        if let quality = shell.library.quality {
            let choice = quality.choice(slot)
            SettingsRow(label: label, hint: hint) {
                HStack(spacing: T.px(8)) {
                    GlassSelect(shell: shell, label: L("{label} format", ["label": label]), value: choice.format, options: formatOptions(quality), width: T.px(140)) { value in
                        quality.set(slot, format: value, kbps: value == "original" ? 0 : quality.defaultKbps(format: value))
                    }
                    if choice.format == "original" {
                        DisabledPill(value: L("Source"), width: T.px(140))
                    } else {
                        GlassSelect(shell: shell, label: L("{label} maximum bitrate", ["label": label]), value: String(choice.kbps), options: quality.bitrates(format: choice.format).map { SelectOption(value: String($0), label: "\(fmtInt(Int($0))) kbps") }, width: T.px(140)) { value in
                            quality.set(slot, format: choice.format, kbps: UInt32(value) ?? choice.kbps)
                        }
                    }
                }
                .fixedSize()
                .onAppear { quality.checkServerOnce() }
            }
        } else {
            SettingsRow(label: label, hint: L("Needs a connection to the server."), dim: true) { DisabledPill(value: L("Original")) }
        }
    }

    private func formatOptions(_ quality: QualitySettings) -> [SelectOption] {
        var options = [SelectOption(value: "original", label: L("Original"))]
        for format in quality.formats where !quality.unavailable.contains(format.id) {
            options.append(SelectOption(value: format.id, label: format.label))
        }
        return options
    }
}

struct StreamingQualityRow: View {
    let shell: ShellModel
    var body: some View { QualityRow(shell: shell, label: L("Streaming quality"), hint: streamHint, slot: .streaming) }
}

struct MeteredQualityRow: View {
    let shell: ShellModel
    private var hint: String {
        let state = shell.library.quality.map { $0.connectionMetered ? L("This connection counts as metered.") : L("This connection doesn't count as metered.") } ?? ""
        return state.isEmpty ? L("Used instead of Streaming quality when your Mac marks the connection as expensive or constrained, like a phone hotspot.") : L("Used instead of Streaming quality when your Mac marks the connection as expensive or constrained, like a phone hotspot. {state}", ["state": state])
    }
    var body: some View { QualityRow(shell: shell, label: L("On metered connections"), hint: hint, slot: .metered) }
}

struct DownloadQualityRow: View {
    let shell: ShellModel
    var body: some View { QualityRow(shell: shell, label: L("Download quality"), hint: downloadHint, slot: .download) }
}

/// off plays every file unchanged.
struct VolumeLevelingRow: View {
    let shell: ShellModel

    var body: some View {
        if let quality = shell.library.quality {
            SettingsRow(label: L("Volume leveling"), hint: levelingHint) {
                GlassSelect(shell: shell, label: L("Volume leveling"), value: quality.leveling, options: [SelectOption(value: "off", label: L("Off")), SelectOption(value: "album", label: L("Album")), SelectOption(value: "track", label: L("Track"))], width: T.px(240)) { quality.setLeveling($0) }
            }
        } else {
            SettingsRow(label: L("Volume leveling"), hint: L("Needs a connection to the server."), dim: true) { DisabledPill(value: L("Off")) }
        }
    }
}
