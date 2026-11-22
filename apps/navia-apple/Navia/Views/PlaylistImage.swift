import AppKit
import UniformTypeIdentifiers

@MainActor func choosePlaylistImage() -> URL? {
    let panel = NSOpenPanel()
    panel.canChooseDirectories = false
    panel.allowsMultipleSelection = false
    panel.allowedContentTypes = [.jpeg, .png, .gif, .webP]
    panel.message = L("Choose an image for this playlist")
    return panel.runModal() == .OK ? panel.url : nil
}

/// uploads the picked file and reports the result in a toast.
@MainActor func pickPlaylistImage(playlist id: String, shell: ShellModel) {
    guard let url = choosePlaylistImage() else { return }
    shell.showToast(L("Uploading image"))
    Task {
        let problem = await shell.library.setPlaylistImage(id: id, path: url.path)
        shell.showToast(problem ?? L("Updated the cover"))
    }
}

@MainActor func removePlaylistImage(playlist id: String, shell: ShellModel) {
    shell.showToast(L("Removing image"))
    Task {
        let problem = await shell.library.removePlaylistImage(id: id)
        shell.showToast(problem ?? L("Removed the image"))
    }
}
