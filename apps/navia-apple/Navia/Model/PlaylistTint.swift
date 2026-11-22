import AppKit
import CoreGraphics
import SwiftUI

/// when this changes, the page tint is read again.
nonisolated enum PlaylistCoverSource: Hashable, Sendable {
    case image(URL)
    case grid([URL])
    case none

    init(_ playlist: PlaylistRow) {
        self.init(cover: playlist.coverURL, collage: playlist.collage)
    }

    /// A server cover, else the album grid.
    init(cover: URL? = nil, collage: [URL]) {
        if let cover {
            self = .image(cover)
        } else if collage.count == 1 {
            self = .image(collage[0])
        } else if !collage.isEmpty {
            self = .grid(collage)
        } else {
            self = .none
        }
    }
}

/// Tints read from playlist covers, by the rule album pages use.
@MainActor final class PlaylistTints {
    static let shared = PlaylistTints()
    private var known: [PlaylistCoverSource: String] = [:]
    private init() {}

    /// Clears on account change, since ids are per server.
    func reset() { known.removeAll() }

    func cached(_ source: PlaylistCoverSource) -> String? { known[source] }

    /// The tint for what the cover shows, or nil if there's no art or it couldn't be read yet.
    func read(_ source: PlaylistCoverSource) async -> String? {
        if let hit = known[source] { return hit }
        var hex: String?
        switch source {
        case .none: return nil
        case .image(let url):
            if url.scheme == CoreArt.scheme {
                _ = await CoreArt.shared.file(for: url, pixels: 512)
                hex = CoreArt.shared.tint(for: url)
            } else if let image = await ArtworkLoader.shared.load(url, pixels: 128) {
                hex = Self.tint { context, side in Self.draw(image, in: CGRect(x: 0, y: 0, width: side, height: side), context) }
            }
        case .grid(let urls):
            var images: [NSImage] = []
            for url in urls {
                guard let image = await ArtworkLoader.shared.load(url, pixels: 128) else { return nil }
                images.append(image)
            }
            let cells = PlaylistCover.gridCells(images)
            hex = Self.tint { context, side in
                let cell = side / 2
                for (index, image) in cells.enumerated() {
                    // Core Graphics counts from the bottom, so the first row of the grid is drawn at the top.
                    Self.draw(image, in: CGRect(x: CGFloat(index % 2) * cell, y: index < 2 ? cell : 0, width: cell, height: cell), context)
                }
            }
        }
        if let hex { known[source] = hex }
        return hex
    }

    private static func draw(_ image: NSImage, in rect: CGRect, _ context: CGContext) {
        guard let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else { return }
        // Cover art is drawn filling its square, as `ArtworkView` does.
        let scale = max(rect.width / CGFloat(cg.width), rect.height / CGFloat(cg.height))
        let (w, h) = (CGFloat(cg.width) * scale, CGFloat(cg.height) * scale)
        context.saveGState()
        context.clip(to: rect)
        context.draw(cg, in: CGRect(x: rect.midX - w / 2, y: rect.midY - h / 2, width: w, height: h))
        context.restoreGState()
    }

    private static let side = 32

    /// Draws at the size the core's `dominant_tint` samples, then applies its rule.
    private static func tint(_ paint: (CGContext, CGFloat) -> Void) -> String? {
        guard let context = CGContext(data: nil, width: side, height: side, bitsPerComponent: 8, bytesPerRow: side * 4,
                                      space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue),
              let raw = context.data else { return nil }
        context.interpolationQuality = .medium
        paint(context, CGFloat(side))
        let bytes = Array(UnsafeBufferPointer(start: raw.assumingMemoryBound(to: UInt8.self), count: side * side * 4))
        return dominantTintHex(rgba: bytes)
    }
}

/// The core's `dominant_tint`, for covers composed on the device: the strongest hue bin weighted by saturation and
/// brightness, or the average for grayscale art.
nonisolated func dominantTintHex(rgba: [UInt8]) -> String {
    let bins = 24
    var weight = [Float](repeating: 0, count: bins)
    var color = [[Float]](repeating: [0, 0, 0], count: bins)
    var total: [Float] = [0, 0, 0]
    var count: Float = 0
    for at in stride(from: 0, to: rgba.count - 3, by: 4) where rgba[at + 3] >= 128 {
        let a = Float(rgba[at + 3]) / 255
        let (r, g, b) = (Float(rgba[at]) / a, Float(rgba[at + 1]) / a, Float(rgba[at + 2]) / a)
        total[0] += r; total[1] += g; total[2] += b
        count += 1
        let (top, low) = (max(r, g, b), min(r, g, b))
        let chroma = top - low
        if chroma < 16 { continue }
        var hue: Float
        if top == r {
            hue = ((g - b) / chroma).truncatingRemainder(dividingBy: 6)
            if hue < 0 { hue += 6 }
        } else if top == g {
            hue = (b - r) / chroma + 2
        } else {
            hue = (r - g) / chroma + 4
        }
        hue /= 6
        let bin = min(Int(hue * Float(bins)), bins - 1)
        let w = (chroma / max(top, 1)) * (top / 255)
        weight[bin] += w
        color[bin][0] += r * w; color[bin][1] += g * w; color[bin][2] += b * w
    }
    var out: [Int]
    if let best = weight.indices.max(by: { weight[$0] < weight[$1] }), weight[best] > 0 {
        out = color[best].map { Int($0 / weight[best]) }
    } else if count > 0 {
        out = total.map { Int($0 / count) }
    } else {
        out = [0, 0, 0]
    }
    out = out.map { min(255, max(0, $0)) }
    return String(format: "%02x%02x%02x", out[0], out[1], out[2])
}

/// A playlist page's tint from the cover it shows. Neutral until known, then fades in, and cross-fades when the
/// cover changes.
struct PlaylistPageTint: View {
    let id: String
    let source: PlaylistCoverSource
    /// The last tint this page showed, and for which playlist, so another playlist doesn't inherit it.
    @State private var held: (id: String, hex: String?)?

    init(playlist: PlaylistRow) {
        id = playlist.id
        source = PlaylistCoverSource(playlist)
    }

    init(id: String, source: PlaylistCoverSource) {
        self.id = id
        self.source = source
    }

    var body: some View {
        let hex = source == .none ? nil : PlaylistTints.shared.cached(source) ?? (held?.id == id ? held?.hex : nil)
        ZStack(alignment: .top) {
            if let hex { PageTint(hex: hex).id(hex).transition(.opacity) }
        }
        .animation(T.easeOut(T.tintFade), value: hex)
        .task(id: source) {
            let id = id
            var delay: Double = 2
            while !Task.isCancelled {
                if source == .none {
                    held = (id, nil)
                    return
                }
                if let found = await PlaylistTints.shared.read(source) {
                    held = (id, found)
                    return
                }
                try? await Task.sleep(for: .seconds(delay))
                delay = min(60, delay * 2)
            }
        }
    }
}
