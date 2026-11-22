import Foundation

/// The colors derived from a piece of artwork, matching GPUI's.
struct Tint: Equatable, Sendable {
    let vivid: RGBA
    let fg: RGBA
    let fgDim: RGBA
    /// The bar's glow, at a fixed lightness so dark covers still glow.
    let barGlow: RGBA
    /// The player's background: `vivid` with its lightness capped at .47 instead of .6, so light covers keep 4.5:1 under
    /// the player's text.
    let playerVivid: RGBA
    /// The brightest ink: the transport glyphs and the progress and volume fills.
    let playerFg: RGBA
    /// The dimmer ink, fully opaque: the artist, the times and the volume row's icons.
    let playerFgDim: RGBA

    init(hex: String?) {
        let base = hex.flatMap(RGBA.init(hex:)) ?? T.tintFallback
        let (l, c, h) = OKLCH.from(base)
        vivid = OKLCH.rgb(l: min(0.6, max(0.27, l + 0.03)), c: min(0.15, c * 1.25), h: h)
        fg = OKLCH.rgb(l: 0.86, c: min(0.13, c * 1.5), h: h)
        fgDim = OKLCH.rgb(l: 0.88, c: 0.06, h: h)
        barGlow = OKLCH.rgb(l: 0.62, c: min(0.15, max(0.05, c * 1.7)), h: h)
        // Chroma follows the cover's, so a gray cover stays gray.
        playerVivid = OKLCH.rgb(l: min(T.tintTextSafeL, max(0.27, l + 0.03)), c: min(0.15, c * 1.25), h: h)
        playerFg = OKLCH.rgb(l: 0.90, c: min(0.13, c * 1.8), h: h)
        playerFgDim = OKLCH.rgb(l: 0.85, c: min(0.11, c * 1.6), h: h)
    }
}

enum OKLCH {
    private static func linear(_ c: Double) -> Double { c <= 0.04045 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4) }
    private static func gamma(_ c: Double) -> Double { c <= 0.0031308 ? 12.92 * c : 1.055 * pow(c, 1 / 2.4) - 0.055 }

    static func from(_ color: RGBA) -> (l: Double, c: Double, h: Double) {
        let (r, g, b) = (linear(color.r), linear(color.g), linear(color.b))
        let l1 = cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b)
        let m1 = cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b)
        let s1 = cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b)
        let L = 0.2104542553 * l1 + 0.7936177850 * m1 - 0.0040720468 * s1
        let A = 1.9779984951 * l1 - 2.4285922050 * m1 + 0.4505937099 * s1
        let B = 0.0259040371 * l1 + 0.7827717662 * m1 - 0.8086757660 * s1
        return (L, hypot(A, B), atan2(B, A))
    }

    static func rgb(l: Double, c: Double, h: Double) -> RGBA {
        let A = c * Foundation.cos(h), B = c * Foundation.sin(h)
        let l1 = l + 0.3963377774 * A + 0.2158037573 * B
        let m1 = l - 0.1055613458 * A - 0.0638541728 * B
        let s1 = l - 0.0894841775 * A - 1.2914855480 * B
        let (l3, m3, s3) = (l1 * l1 * l1, m1 * m1 * m1, s1 * s1 * s1)
        let r = 4.0767416621 * l3 - 3.3077115913 * m3 + 0.2309699292 * s3
        let g = -1.2684380046 * l3 + 2.6097574011 * m3 - 0.3413193965 * s3
        let b = -0.0041960863 * l3 - 0.7034186147 * m3 + 1.7076147010 * s3
        func clip(_ v: Double) -> Double { min(1, max(0, gamma(max(0, v)))) }
        return RGBA(clip(r), clip(g), clip(b))
    }

    /// whether the colour is inside sRGB before any clipping.
    static func inSRGB(l: Double, c: Double, h: Double) -> Bool {
        let A = c * Foundation.cos(h), B = c * Foundation.sin(h)
        let l1 = l + 0.3963377774 * A + 0.2158037573 * B
        let m1 = l - 0.1055613458 * A - 0.0638541728 * B
        let s1 = l - 0.0894841775 * A - 1.2914855480 * B
        let (l3, m3, s3) = (l1 * l1 * l1, m1 * m1 * m1, s1 * s1 * s1)
        let rgb = [4.0767416621 * l3 - 3.3077115913 * m3 + 0.2309699292 * s3,
                   -1.2684380046 * l3 + 2.6097574011 * m3 - 0.3413193965 * s3,
                   -0.0041960863 * l3 - 0.7034186147 * m3 + 1.7076147010 * s3]
        return rgb.allSatisfy { $0 >= -1e-4 && $0 <= 1 + 1e-4 }
    }
}
