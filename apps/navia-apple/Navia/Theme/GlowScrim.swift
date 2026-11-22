import AppKit
import SwiftUI

/// The expanded player's text scrim. The blurred artwork can light the backdrop under the controls, so a soft black
/// scrim sits behind them, only as dark as `playerFgDim` needs for 3:1 and `playerFg` for 4.5:1, and never darker
/// than `T.glowScrimMax`. Dark covers get none.
///
/// The backdrop is modeled from the cover as `PlayerExpanded` draws it.
enum GlowScrim {
    static let grid = 32
    /// 3:1 for the dim text and 4.5:1 for the controls, plus a little room, since SwiftUI's blur and saturation don't
    /// match the model exactly.
    static let dimContrast = 3.1
    static let fgContrast = 4.6

    /// The cover at `grid` x `grid`, RGBA bytes in sRGB.
    static func pixels(_ image: NSImage) -> [UInt8]? {
        guard let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil),
              let context = CGContext(data: nil, width: grid, height: grid, bitsPerComponent: 8, bytesPerRow: grid * 4,
                                      space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue),
              let raw = context.data else { return nil }
        context.interpolationQuality = .high
        context.draw(cg, in: CGRect(x: 0, y: 0, width: grid, height: grid))
        // A bitmap context's buffer starts at the image's top row, like a canvas.
        return Array(UnsafeBufferPointer(start: raw.assumingMemoryBound(to: UInt8.self), count: grid * grid * 4))
    }

    /// The brightest luminance the backdrop has under `rect`, in a window of `size` (points), glow blurred by `blur`.
    static func peak(cover: [UInt8], tint: Tint, size: CGSize, rect: CGRect, blur: Double) -> Double {
        let n = grid, W = Double(size.width), H = Double(size.height)
        guard cover.count >= n * n * 4, W > 0, H > 0, rect.width > 0, rect.height > 0 else { return 0 }
        let stops = backdropStops(tint)
        let (c0, vivid, c2) = (stops[0].color, stops[1].color, stops[2].color)
        let scale = max(1.4 * W, 1.4 * H) / Double(n)
        let ox = -0.2 * W + (1.4 * W - Double(n) * scale) / 2, oy = -0.2 * H + (1.4 * H - Double(n) * scale) / 2
        var glow = (0..<(n * n)).map { [Double(cover[$0 * 4]) / 255, Double(cover[$0 * 4 + 1]) / 255, Double(cover[$0 * 4 + 2]) / 255] }
        glow = gaussian(glow, n: n, sigma: blur / scale)
        func at(_ u: Double, _ v: Double) -> [Double] {
            let x = min(Double(n - 1), max(0, u)), y = min(Double(n - 1), max(0, v))
            let x0 = Int(x), y0 = Int(y), x1 = min(n - 1, x0 + 1), y1 = min(n - 1, y0 + 1), fx = x - Double(x0), fy = y - Double(y0)
            return (0..<3).map { i in
                let top = glow[y0 * n + x0][i] * (1 - fx) + glow[y0 * n + x1][i] * fx
                let bottom = glow[y1 * n + x0][i] * (1 - fx) + glow[y1 * n + x1][i] * fx
                return top * (1 - fy) + bottom * fy
            }
        }
        var peak = 0.0
        let steps = 8
        for j in 0...steps {
            for i in 0...steps {
                let px = Double(rect.minX) + Double(rect.width) * Double(i) / Double(steps)
                let py = Double(rect.minY) + Double(rect.height) * Double(j) / Double(steps)
                // SwiftUI's gradient runs from the top-leading to the bottom-trailing corner.
                let t = min(1, max(0, (px * W + py * H) / (W * W + H * H)))
                let base = t < 0.42 ? c0.mixed(with: vivid, amount: 1 - t / 0.42) : vivid.mixed(with: c2, amount: 1 - (t - 0.42) / 0.58)
                let g = saturate(at((px - ox) / scale - 0.5, (py - oy) / scale - 0.5), T.playerGlowSaturation).map { $0 * T.playerGlowBrightness }
                let o = T.playerGlowOpacity
                let mixed = RGBA(g[0] * o + base.r * (1 - o), g[1] * o + base.g * (1 - o), g[2] * o + base.b * (1 - o))
                peak = max(peak, mixed.luminance)
            }
        }
        return peak
    }

    /// the expanded player's backdrop gradient, as `PlayerExpanded` draws it.
    static func backdropStops(_ tint: Tint) -> [(color: RGBA, location: Double)] {
        let vivid = tint.playerVivid
        return [(vivid.mixed(with: T.playerShadeStart, amount: T.playerShadeMix), 0), (vivid, 0.42), (vivid.mixed(with: T.playerShadeEnd, amount: 0.58), 1)]
    }

    /// none when the backdrop is dark enough, else what brings its peak down to the targets.
    static func alpha(peak: Double, tint: Tint) -> Double {
        let target = min((tint.playerFgDim.luminance + 0.05) / dimContrast, (tint.playerFg.luminance + 0.05) / fgContrast) - 0.05
        guard peak > target else { return 0 }
        // A black layer scales sRGB values, and luminance follows at about the 2.2 power.
        return min(T.glowScrimMax, ((1 - pow(target / peak, 1 / 2.2)) * 100).rounded(.up) / 100)
    }

    private static func saturate(_ c: [Double], _ s: Double) -> [Double] {
        let (r, g, b) = (c[0], c[1], c[2])
        return [(0.213 + 0.787 * s) * r + (0.715 - 0.715 * s) * g + (0.072 - 0.072 * s) * b,
                (0.213 - 0.213 * s) * r + (0.715 + 0.285 * s) * g + (0.072 - 0.072 * s) * b,
                (0.213 - 0.213 * s) * r + (0.715 - 0.715 * s) * g + (0.072 + 0.928 * s) * b].map { min(1, max(0, $0)) }
    }

    private static func gaussian(_ grid: [[Double]], n: Int, sigma: Double) -> [[Double]] {
        guard sigma >= 0.3 else { return grid }
        let radius = Int((sigma * 3).rounded(.up))
        let weights = (-radius...radius).map { exp(-Double($0 * $0) / (2 * sigma * sigma)) }
        let total = weights.reduce(0, +)
        func pass(_ src: [[Double]], _ dx: Int, _ dy: Int) -> [[Double]] {
            (0..<(n * n)).map { idx in
                let x = idx % n, y = idx / n
                var out = [0.0, 0.0, 0.0]
                for i in -radius...radius {
                    let sx = min(n - 1, max(0, x + i * dx)), sy = min(n - 1, max(0, y + i * dy))
                    let w = weights[i + radius] / total
                    for k in 0..<3 { out[k] += src[sy * n + sx][k] * w }
                }
                return out
            }
        }
        return pass(pass(grid, 1, 0), 0, 1)
    }
}

extension EnvironmentValues {
    /// The player's dimmer ink for the artist, the times and the volume row. Nil outside the player.
    @Entry var playerDim: Color? = nil
}
