import AppKit
import ImageIO
import SwiftUI

/// decodes off the main thread, downsampled to the shown size, and caches the result.
final class ArtworkLoader {
    static let shared = ArtworkLoader()

    private let cache = NSCache<NSString, NSImage>()
    private init() { cache.countLimit = 800 }

    private func key(_ url: URL, _ pixels: Int) -> NSString { "\(pixels)-\(url.path)" as NSString }

    func cached(_ url: URL, pixels: Int) -> NSImage? { cache.object(forKey: key(url, pixels)) }

    func clear() { cache.removeAllObjects() }

    func load(_ url: URL, pixels: Int) async -> NSImage? {
        if let hit = cached(url, pixels: pixels) { return hit }
        var file = url
        var keep = true
        if url.scheme == CoreArt.scheme {
            guard let resolved = await CoreArt.shared.file(for: url, pixels: pixels) else { return nil }
            file = resolved
            keep = !CoreArt.shared.isPlaceholder(url, pixels: pixels)
        }
        // A web image, such as a Discord avatar, is fetched and then decoded the same way.
        if url.scheme == "https" {
            guard let (data, response) = try? await URLSession.shared.data(from: url),
                  (response as? HTTPURLResponse)?.statusCode == 200 else { return nil }
            let image = await Task.detached(priority: .userInitiated) { Self.decode(data: data, pixels: pixels) }.value
            if let image { cache.setObject(image, forKey: key(url, pixels)) }
            return image
        }
        let image = await Task.detached(priority: .userInitiated) { Self.decode(file, pixels: pixels) }.value
        if let image, keep { cache.setObject(image, forKey: key(url, pixels)) }
        return image
    }

    /// For offscreen check images, where `.task` never runs.
    func loadNow(_ url: URL, pixels: Int) -> NSImage? {
        if let hit = cached(url, pixels: pixels) { return hit }
        guard url.scheme != CoreArt.scheme, url.scheme != "https" else { return nil }
        let image = Self.decode(url, pixels: pixels)
        if let image { cache.setObject(image, forKey: key(url, pixels)) }
        return image
    }

    nonisolated private static func decode(_ url: URL, pixels: Int) -> NSImage? {
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
        return thumbnail(source, pixels: pixels)
    }

    nonisolated private static func decode(data: Data, pixels: Int) -> NSImage? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        return thumbnail(source, pixels: pixels)
    }

    nonisolated private static func thumbnail(_ source: CGImageSource, pixels: Int) -> NSImage? {
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceThumbnailMaxPixelSize: pixels,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true,
        ]
        guard let cg = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else { return nil }
        return NSImage(cgImage: cg, size: NSSize(width: cg.width, height: cg.height))
    }
}

enum ArtworkKind {
    case album, artist, station

    fileprivate var glyph: String {
        switch self {
        case .album: "music.note"
        case .artist: "music.microphone"
        case .station: "dot.radiowaves.left.and.right"
        }
    }
}

/// like CSS `object-fit: cover`; a missing or broken image gets the plain fallback.
struct ArtworkView: View {
    let url: URL?
    var kind: ArtworkKind = .album
    var pixels = 900
    var alignment: Alignment = .center
    /// The vertical focus, 0 to 1, like CSS `object-position`. Nil uses `alignment`.
    var focusY: CGFloat? = nil
    /// only where art is large.
    var animates = false
    @State private var loaded: (url: URL, image: NSImage)?
    @State private var frames: AnimatedFrames?
    @Environment(\.isSnapshot) private var isSnapshot
    @Environment(\.motionLive) private var live

    /// art already in memory shows from the first frame; art that arrives later fades in (`T.artFade`).
    private var shown: NSImage? {
        guard let url else { return nil }
        if isSnapshot { return ArtworkLoader.shared.loadNow(url, pixels: pixels) }
        if let loaded, loaded.url == url { return loaded.image }
        return ArtworkLoader.shared.cached(url, pixels: pixels)
    }

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: alignment) {
                if let shown {
                    picture(shown, in: geo.size).transition(.opacity)
                    // under Reduce Motion the still cover stays: it is the first frame.
                    if let frames, focusY == nil, !Motion.shared.reduced {
                        AnimatedArtLayer(frames: frames, alignment: alignment)
                            .frame(width: geo.size.width, height: geo.size.height)
                    }
                } else {
                    fallback(in: geo.size)
                }
            }
            .frame(width: geo.size.width, height: geo.size.height, alignment: alignment)
            .clipped()
        }
        .task(id: url) {
            guard let url else { loaded = nil; return }
            let wasShown = ArtworkLoader.shared.cached(url, pixels: pixels) != nil
            var delay = 2.0
            while !Task.isCancelled {
                if let image = await ArtworkLoader.shared.load(url, pixels: pixels) {
                    withAnimation(wasShown || !live ? nil : T.ease(T.artFade)) { loaded = (url, image) }
                    return
                }
                // Nothing is remembered as missing: a request can be dropped, or the network can blip.
                loaded = nil
                guard url.scheme == CoreArt.scheme else { return }
                try? await Task.sleep(for: .seconds(delay))
                delay = min(delay * 2, 60)
            }
        }
        .task(id: animates && !isSnapshot ? url : nil) {
            frames = nil
            guard animates, !isSnapshot, let url else { return }
            frames = await CoreArt.shared.animatedFrames(for: url, pixels: pixels)
        }
    }

    @ViewBuilder private func picture(_ image: NSImage, in size: CGSize) -> some View {
        let fill = Image(nsImage: image).resizable().interpolation(.high).aspectRatio(contentMode: .fill)
        if let focusY, image.size.height > 0 {
            let height = max(size.height, size.width * image.size.height / image.size.width)
            fill.frame(width: size.width, height: height)
                .offset(y: (size.height - height) * focusY)
                .frame(width: size.width, height: size.height, alignment: .top)
        } else {
            fill.frame(width: size.width, height: size.height, alignment: alignment)
        }
    }

    private func fallback(in size: CGSize) -> some View {
        T.panel2.color.overlay(
            Image(systemName: kind.glyph)
                .font(.system(size: max(10, size.width * T.artGlyph), weight: .regular))
                .foregroundStyle(T.t3.color)
        )
    }
}
