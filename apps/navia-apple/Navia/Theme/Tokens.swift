import AppKit
import Synchronization
import SwiftUI

/// A color usable from both SwiftUI and AppKit.
struct RGBA: Equatable, Sendable {
    var r, g, b, a: Double

    init(_ r: Double, _ g: Double, _ b: Double, _ a: Double = 1) {
        self.r = r; self.g = g; self.b = b; self.a = a
    }

    /// `#rrggbb`, with or without the leading `#`.
    init?(hex: String) {
        let s = hex.hasPrefix("#") ? String(hex.dropFirst()) : hex
        guard s.count == 6, let v = UInt32(s, radix: 16) else { return nil }
        self.init(Double((v >> 16) & 255) / 255, Double((v >> 8) & 255) / 255, Double(v & 255) / 255)
    }

    /// CSS `rgb(r g b / a)` with 0-255 channels.
    static func css(_ r: Double, _ g: Double, _ b: Double, _ a: Double = 1) -> RGBA {
        RGBA(r / 255, g / 255, b / 255, a)
    }

    /// CSS `hsl(h s% l%)`.
    static func hsl(_ h: Double, _ s: Double, _ l: Double) -> RGBA {
        let hue = h.truncatingRemainder(dividingBy: 360) + (h < 0 ? 360 : 0)
        let c = (1 - abs(2 * l - 1)) * s
        let x = c * (1 - abs((hue / 60).truncatingRemainder(dividingBy: 2) - 1))
        let m = l - c / 2
        let (r, g, b): (Double, Double, Double)
        switch hue {
        case ..<60: (r, g, b) = (c, x, 0)
        case ..<120: (r, g, b) = (x, c, 0)
        case ..<180: (r, g, b) = (0, c, x)
        case ..<240: (r, g, b) = (0, x, c)
        case ..<300: (r, g, b) = (x, 0, c)
        default: (r, g, b) = (c, 0, x)
        }
        return RGBA(r + m, g + m, b + m)
    }

    var color: Color { Color(.sRGB, red: r, green: g, blue: b, opacity: a) }
    var ns: NSColor { NSColor(srgbRed: r, green: g, blue: b, alpha: a) }

    func opacity(_ value: Double) -> RGBA { RGBA(r, g, b, value) }

    /// CSS `color-mix(in srgb, self amount, other)`.
    func mixed(with other: RGBA, amount: Double) -> RGBA {
        RGBA(r * amount + other.r * (1 - amount), g * amount + other.g * (1 - amount), b * amount + other.b * (1 - amount), a * amount + other.a * (1 - amount))
    }

    var luminance: Double {
        func channel(_ c: Double) -> Double { c <= 0.03928 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4) }
        return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
    }

    func contrast(against other: RGBA) -> Double {
        let (a, b) = (luminance, other.luminance)
        return (max(a, b) + 0.05) / (min(a, b) + 0.05)
    }

    /// APCA (SAPC/APCA-W3 0.0.98G) constants for perceptual lightness contrast. WCAG 2 ratios favor black on
    /// mid-tones, where white reads better. GPUI's color.rs uses the same constants.
    private enum APCA {
        static let trc = 2.4, rCo = 0.2126729, gCo = 0.7151522, bCo = 0.072175
        static let blkThrs = 0.022, blkClmp = 1.414
        static let normBG = 0.56, normTXT = 0.57, revTXT = 0.62, revBG = 0.65
        static let scale = 1.14, loOffset = 0.027, loClip = 0.1, deltaYmin = 0.0005
    }

    /// APCA's screen luminance, with its soft clamp near black.
    private var apcaY: Double {
        let y = APCA.rCo * pow(r, APCA.trc) + APCA.gCo * pow(g, APCA.trc) + APCA.bCo * pow(b, APCA.trc)
        return y < APCA.blkThrs ? y + pow(APCA.blkThrs - y, APCA.blkClmp) : y
    }

    /// APCA lightness contrast (Lc) as text on `background`: positive for dark on light, negative for light on dark.
    func apcaLc(on background: RGBA) -> Double {
        let (t, b) = (apcaY, background.apcaY)
        if abs(b - t) < APCA.deltaYmin { return 0 }
        if b > t {
            let s = (pow(b, APCA.normBG) - pow(t, APCA.normTXT)) * APCA.scale
            return s < APCA.loClip ? 0 : (s - APCA.loOffset) * 100
        }
        let s = (pow(b, APCA.revBG) - pow(t, APCA.revTXT)) * APCA.scale
        return s > -APCA.loClip ? 0 : (s + APCA.loOffset) * 100
    }
}

/// The design tokens, with the same names and meanings as GPUI's `theme.rs`. Sizes are CSS pixels rendered at
/// 87.5%, so every size goes through `px`: 16 becomes 14 points.
///
/// `zoom` is the Interface size setting, 1 at 100%. `RootView` rebuilds the page when it changes.
enum T {
    static var zoom: CGFloat = 1
    static var scale: CGFloat { 0.875 * zoom }
    static func px(_ value: CGFloat) -> CGFloat { value * scale }

    static let bg = RGBA(hex: "171718")!
    static let panel = RGBA(hex: "1f1f21")!
    static let panelGlass = RGBA.css(31, 31, 33, 0.7)
    /// The page color inside the sidebar's glass, Mac only. The system glass lifts itself much more while the window
    /// is inactive, so this keeps the sidebar a few levels above the window color either way.
    static let panelGlassVeil = bg.opacity(0.25)
    static let panelGlassVeilInactive = bg.opacity(0.6)
    static let panel2 = RGBA(hex: "2a2a2d")!
    static let row = RGBA.css(255, 255, 255, 0.07)
    static let danger = RGBA(hex: "e5484d")!
    static let hover = RGBA.css(255, 255, 255, 0.10)
    /// Small controls brighten their ink on hover instead of taking a fill, and dim when pressed. Play and skip
    /// grow instead, and shrink when pressed.
    static let hoverInk = t1
    static let pressDim = 0.7
    /// A disabled control keeps its shape and colors at this opacity.
    static let disabledOpacity = 0.4
    /// The player's mode buttons at rest, and disabled.
    static let modeRest = 0.62
    static let modeDisabled = 0.35
    /// `--icon-rest`: an icon button's resting opacity. `switcherRest`: an unselected player view switcher segment.
    static let iconRest = 0.88
    static let switcherRest = 0.85
    /// Play and skip don't grow while motion is reduced (`Motion`).
    nonisolated static var hoverGrow: CGFloat { Motion.isReduced ? 1 : 1.08 }
    nonisolated static let pressShrink: CGFloat = 0.94
    /// `PressPhase` scales, in the bar and the full screen player.
    nonisolated static let pressHold: CGFloat = 0.86
    nonisolated static let pressSquash: CGFloat = 0.72
    nonisolated static let pressOver: CGFloat = 1.08
    /// `--press`: a hovered button while it's held down.
    static let press = RGBA.css(255, 255, 255, 0.16)
    /// `--active-fill`, `--active-fill-size`, `--active-fill-pad`: behind a held player control (`ActiveFill`).
    static let activeFill = RGBA(0, 0, 0, 0.22)
    static let activeFillSize: CGFloat = 1.7
    /// `--active-fill-start`, `--active-fill-grow`, `--active-fill-fade`: the held fill's starting share of its size, and seconds.
    nonisolated static let activeFillStart: CGFloat = 0.9
    nonisolated static let activeFillGrow = 0.25
    nonisolated static let activeFillFade = 0.25
    static var activeFillPad: CGFloat { px(3) }
    static let line = RGBA.css(255, 255, 255, 0.08)

    /// Text and icon ink: 90%, 68% and 57% white.
    static let t1 = RGBA.css(255, 255, 255, 0.90)
    static let t2 = RGBA.css(255, 255, 255, 0.68)
    static let t3 = RGBA.css(255, 255, 255, 0.57)
    /// Pure white for titles, the selected sidebar row, tab and segment, and the sorted column.
    static let tEm = RGBA(1, 1, 1)
    /// `--t-nav`: sidebar rows, label and icon in one ink, and the sidebar's other small labels. `--t-nav-section`: the
    /// sidebar's headings and their chevrons. `--t-section`: other section headings.
    static let tNav = tEm
    static let tNavSection = tEm
    static let tSection = t3
    /// Text and icons over artwork or the cover-tinted player, and slider thumbs.
    static let tOnArt = RGBA(1, 1, 1)
    static let knob = RGBA(1, 1, 1)
    /// The bottom bar is neutral, never the cover tint: its title, its glyphs and secondary text, and its
    /// unplayed track.
    static let barTitle = t1
    static let barInk = t2
    static let barTrack = RGBA.css(255, 255, 255, 0.22)
    /// The unplayed track in the dock and the expanded player.
    static let playerTrack = RGBA.css(0, 0, 0, 0.2)
    /// The expanded player's text scrim: wide, soft and light so it never reads as a box.
    static var glowScrimReach: CGFloat { px(96) }
    static var glowScrimBlur: CGFloat { px(64) }
    static let glowScrimMax = 0.32

    static let glass = RGBA.css(58, 58, 62, 0.46)
    static let glassOn = RGBA.css(255, 255, 255, 0.12)
    static let glassEdge = RGBA.css(255, 255, 255, 0.16)
    /// A flat glass fill under the pointer and held down, and the line between segments of a glass group.
    static let glassHover = RGBA.css(92, 92, 98, 0.52)
    static let glassPress = RGBA.css(112, 112, 118, 0.58)
    static let glassDivider = RGBA.css(255, 255, 255, 0.22)
    /// Behind a dialog and the sidebar drawer.
    static let scrim = RGBA(0, 0, 0, 0.45)
    /// the sign-in and prompt cards' tint; for the surfaces on the system's glass (menus, popovers, dialogs, the bar),
    /// only offscreen check images draw it.
    static let floatGlass = RGBA.css(38, 38, 42, 0.82)
    /// `--menu-glass`: menus and popovers; on the system's glass, so only offscreen check images draw it.
    static let menuGlass = RGBA.css(95, 95, 100, 0.41)
    /// download marks (`--download-*`): the ring's track, a waiting download, the mark, the glyph and the gap before a mark.
    static let downloadTrack = RGBA.css(255, 255, 255, 0.22)
    static var downloadPaused: RGBA { t3 }
    static var downloadMark: CGFloat { px(14) }
    static var downloadGlyph: CGFloat { px(20) }
    static var downloadMarkGap: CGFloat { px(6) }
    /// The room above each queue section heading.
    static var queueSectionGap: CGFloat { px(14) }
    /// Placeholder rows under the Autoplay heading while its songs are being found.
    static let queueFindingRows = 3
    /// A placeholder row's two text lines as quiet blocks: height, gap, and widths as fractions of the row.
    static var rowPlaceholderLine: CGFloat { px(9) }
    static var rowPlaceholderGap: CGFloat { px(8) }
    static let rowPlaceholderTitleW: CGFloat = 0.55
    static let rowPlaceholderArtistW: CGFloat = 0.35
    /// The square X that removes a queue row.
    static var queueRemoveW: CGFloat { px(28) }
    /// The queue headings' icon buttons (Clear, Shuffle): glyph size and gap.
    static var queueHeadIcon: CGFloat { px(18) }
    /// Listening Now rows: the cover beside each person, and the live mark's three accent bars.
    static var personArt: CGFloat { px(30) }
    static var liveMarkH: CGFloat { px(10) }
    static var liveBarW: CGFloat { px(2) }
    static var liveBarGap: CGFloat { px(1.5) }
    /// A Spotify friend's row: the friend mark on the avatar's corner, and initials when there's no avatar.
    static var friendMark: CGFloat { px(13) }
    static var friendMarkGlyph: CGFloat { px(8) }
    static var friendInitials: CGFloat { px(11) }
    static var queueHeadGap: CGFloat { px(8) }
    /// A dialog's width, padding, gaps, and button padding.
    static var dialogW: CGFloat { px(420) }
    /// The sheet variant of a dialog (Get Info) and its info rows.
    static var sheetW: CGFloat { px(560) }
    static var sheetMaxH: CGFloat { px(780) }
    static var settingsMaxW: CGFloat { px(900) }
    static var infoArt: CGFloat { px(88) }
    static var infoLabelW: CGFloat { px(148) }
    static var infoRowPadY: CGFloat { px(9) }
    static var infoRowPadX: CGFloat { px(14) }
    static var infoSectionGap: CGFloat { px(18) }
    /// The cover sheet (an artist's About). Focus and fade positions are fractions of the picture's height.
    static var sheetCoverH: CGFloat { px(420) }
    static var sheetCoverOverlap: CGFloat { px(120) }
    static let sheetCoverFocusY: CGFloat = 0.22
    static let sheetCoverFadeFrom: CGFloat = 0.45
    static var sheetSectionGap: CGFloat { px(22) }
    static var sheetLabelGap: CGFloat { px(8) }
    /// a hovered link's underline and its gap below the baseline.
    static var linkUnderline: CGFloat { px(1) }
    static var linkUnderlineGap: CGFloat { px(2) }
    static var dialogPad: CGFloat { px(22) }
    static var dialogGap: CGFloat { px(14) }
    static var dialogActionsGap: CGFloat { px(10) }
    static var dialogActionsTop: CGFloat { px(4) }
    /// the New Playlist cover row (`--cover-pick-*`).
    static var coverPickSize: CGFloat { px(200) }
    static var coverPickGap: CGFloat { px(14) }
    static var coverPickArrowInset: CGFloat { px(10) }
    static var coverPickRingW: CGFloat { px(3) }
    static var coverPickRingGap: CGFloat { px(3) }
    static var coverPickUploadIcon: CGFloat { px(28) }
    static var coverPickUploadGap: CGFloat { px(8) }
    static var coverPickDot: CGFloat { px(7) }
    static var coverPickDotGap: CGFloat { px(10) }
    static var coverPickPagerTop: CGFloat { px(10) }
    static var coverPickPagerIcon: CGFloat { px(13) }
    /// `--field-pad`: a text field's side padding. `--focus-ring`: the 2 pt accent ring outside a focused field.
    static var fieldPad: CGFloat { px(15) }
    static let focusRingWidth: CGFloat = 2
    /// Every floating panel's corners.
    static var rFloat: CGFloat { px(16) }
    /// A menu's submenu (Add to Playlist). Mac menus are native, so only the width is used, for the filter field.
    static var submenuW: CGFloat { px(260) }
    /// native menus keep the system's size at every Interface size, so these don't go through `px`. the quick row's
    /// squares (`--menu-quick-size`) spread over the content width, never closer than `menuQuickGapRatio` of a square.
    static let menuIcon: CGFloat = 14
    /// the box every menu icon sits in, so titles line up whatever the symbol's width.
    static let menuIconCell = CGSize(width: 20, height: 16)
    /// the widest a menu icon draws; wider symbols scale down to it, so the column's edges line up too.
    static let menuIconFitW: CGFloat = 16
    static let menuContentInset: CGFloat = 14
    /// `--menu-text`, `--menu-detail-text`: the system menu's own sizes, for rows the app draws into a native menu.
    static let menuText: CGFloat = 13
    static let menuDetailText: CGFloat = 11
    /// `--menu-header-w`, `--menu-avatar`, `--menu-gap`: the account menu's header row (`menuHeaderH` tall).
    static let menuHeaderW: CGFloat = 260
    static let menuHeaderH: CGFloat = 48
    static let menuAvatar: CGFloat = 30
    static let menuGap: CGFloat = 10
    static let menuQuickGapRatio: CGFloat = 0.4
    static let menuQuickSize: CGFloat = 34
    static let menuQuickIconRatio: CGFloat = 20.0 / 42
    static let menuQuickPadTop: CGFloat = 5
    static let menuQuickPadBottom: CGFloat = 4
    static let menuQuickRadius: CGFloat = 8
    /// the icon-only menu buttons' own tooltip: delay in seconds when the system sets none, padding, gap under the button.
    static let menuTipDelay = 0.6
    static var menuTipPadX: CGFloat { px(7) }
    static var menuTipPadY: CGFloat { px(3) }
    static var menuTipGap: CGFloat { px(4) }
    static var menuTipRadius: CGFloat { px(5) }
    /// A submenu's filter field; its fill is the label colour at `row`'s opacity.
    static let menuFindH: CGFloat = 24
    static let menuFindSpace: CGFloat = 8
    static let menuFindRadius: CGFloat = 7
    static let menuFindPad: CGFloat = 8
    static let menuFindGlyph: CGFloat = 12
    static let menuFindGap: CGFloat = 6
    /// `--pop-w`, `--pop-max-h`: the anchored popover; its content scrolls past the tallest.
    static var popW: CGFloat { px(380) }
    static var popMaxH: CGFloat { px(440) }
    /// `--quality-pop-*`: the quality badge's details, their widest, padding, column and row gaps, and distance from the
    /// badge and from the window's edge.
    static var qualityPopMaxW: CGFloat { px(400) }
    static var qualityPopPadTop: CGFloat { px(10) }
    static var qualityPopPadX: CGFloat { px(14) }
    static var qualityPopPadBottom: CGFloat { px(11) }
    static var qualityPopColGap: CGFloat { px(14) }
    static var qualityPopRowGap: CGFloat { px(7) }
    static var qualityPopGap: CGFloat { px(8) }
    static var qualityPopEdge: CGFloat { px(10) }
    /// song filters: `--chip-*`, `--filter-*` and `--number-field-w`.
    static var chipH: CGFloat { px(32) }
    static var chipPad: CGFloat { px(13) }
    static var chipGlyph: CGFloat { px(12) }
    static var chipGap: CGFloat { px(6) }
    static var filterRowGap: CGFloat { px(16) }
    static var chipValueMaxW: CGFloat { px(180) }
    static var filterPopW: CGFloat { px(300) }
    static var filterListMaxH: CGFloat { px(300) }
    static var numberFieldW: CGFloat { px(76) }
    /// The smart playlist editor's pills, rows, group indent, sort rows and limit field.
    static var smartFieldW: CGFloat { px(220) }
    static var smartOpW: CGFloat { px(175) }
    static var smartRowGap: CGFloat { px(8) }
    static var smartGroupIndent: CGFloat { px(14) }
    static var smartSortW: CGFloat { px(220) }
    static var smartSortLabelW: CGFloat { px(64) }
    static var smartLimitW: CGFloat { px(140) }
    /// A filter chip that's on: a light fill with dark ink.
    static let chipOn = RGBA.css(255, 255, 255, 1)
    static let onChip = RGBA.css(17, 17, 17, 1)
    /// The hue slider's saturation and lightness, those of the default accent, hsl(218 88.6% 65.7%).
    static let accentS = 0.886
    static let accentL = 0.657
    /// `SYSTEM_ACCENT_MIN_S`: below this saturation the system accent has no real hue (graphite).
    static let systemAccentMinS = 0.1
    /// `--accent-swatch`: an accent swatch in Settings.
    static var accentSwatch: CGFloat { px(26) }
    /// `--accent-wheel-check-shadow`: lifts the check off the System swatch's colour wheel; its colour and blur.
    static let accentWheelCheckShadow = RGBA.css(0, 0, 0, 0.85)
    static var accentWheelCheckBlur: CGFloat { px(2) }
    /// Mac only: room above the sidebar's first row, so the scroll edge doesn't cut the top of its highlight.
    static var sidebarListTop: CGFloat { px(6) }
    /// `--sidebar-heading-top`, `--sidebar-heading-pad-y`: the room above a sidebar heading, and the padding above and below its text (where a foldable one's hover fill reaches).
    static var sidebarHeadingTop: CGFloat { px(18) }
    static var sidebarHeadingPadY: CGFloat { px(7) }
    static var sidebarItemPadX: CGFloat { px(14) }
    /// `--sidebar-cover`, `--sidebar-cover-r`, `--sidebar-cover-glyph`: a sidebar playlist's cover and the play or pause glyph over it.
    static var sidebarCover: CGFloat { px(22) }
    static var sidebarCoverR: CGFloat { px(5) }
    static var sidebarCoverGlyph: CGFloat { px(12) }
    /// `--search-clear`, `--search-clear-lg`, `--search-clear-sm`: the clear button at the end of a search field with text.
    static var searchClear: CGFloat { px(16) }
    static var searchClearLG: CGFloat { px(20) }
    static var searchClearSM: CGFloat { px(14) }
    /// `--search-pill-pad`, `--search-pill-gap`: a page's search pill, its side padding and the gap after its glyph.
    static var searchPillPad: CGFloat { px(15) }
    static var searchPillGap: CGFloat { px(9) }
    /// the system text field's own room each side of its text.
    static let fieldTextInset: CGFloat = 2
    static var settingsSectionGap: CGFloat { px(30) }
    /// `--settings-text-min`: the least a Settings row's text keeps beside its control; narrower, the control goes under the text.
    static var settingsTextMin: CGFloat { px(240) }
    /// `--toast-gap`: how far a toast floats above the bottom of the content column (the window inset, or the bar).
    static var toastGap: CGFloat { px(32) }
    /// A toast's height and side padding. One with an action keeps the height, with a small button at the right.
    static var toastH: CGFloat { px(42) }
    static var toastPad: CGFloat { px(18) }
    /// Between an action button's icon and its label.
    static var buttonGap: CGFloat { px(8) }
    /// The small glass button, such as a toast's Undo.
    static var buttonSmallH: CGFloat { px(30) }
    static var buttonSmallPad: CGFloat { px(12) }
    /// seconds a toast stays; one with an action stays longer, so there's time to reach it.
    static let toast = 3.0
    static let toastAction = 5.0
    /// `--switcher-on`: the switcher's selected view.
    static let switcherOn = RGBA.css(255, 255, 255, 0.20)
    /// A queue row hovered and selected over the tinted player.
    static let onArtHover = RGBA.css(255, 255, 255, 0.12)
    static let onArtSelected = RGBA.css(255, 255, 255, 0.22)
    /// Where artwork or a row hasn't arrived yet over the player.
    static let onArtPlaceholder = RGBA.css(255, 255, 255, 0.08)
    /// What the player gradients darken toward: the expanded player's corners, the dock's bottom, and the fade under
    /// the dock's buttons.
    static let playerShadeStart = RGBA(hex: "081018")!
    static let playerShadeEnd = RGBA(hex: "060a14")!
    static let dockShade = RGBA(hex: "0a0e16")!
    static let dockHeadShade = RGBA(0, 0, 0, 0.34)
    /// How much of the tint stays where the dock and players darken toward a shade.
    static let playerShadeMix = 0.72
    /// The dark fill under window controls drawn over a cover (the large mini player).
    static let windowButtonOnArt = RGBA(0, 0, 0, 0.22)
    static let windowButtonOnArtHover = RGBA(0, 0, 0, 0.35)
    /// `--lyric-peek`, `--lyric-top-clear`: the previous line's room above the current one, as shares of the type size.
    static let lyricPeek: CGFloat = 1.0
    static let lyricTopClear = 0.1
    /// `--lyric-*` by distance d from the current line: opacity `max(min, near - d * step)`, blur `min(max, near + d * step)`.
    static let lyricDimNear = 0.46
    static let lyricDimStep = 0.07
    static let lyricDimMin = 0.14
    static let lyricHover = 0.9
    static let lyricBlurNear: CGFloat = 1.2
    static let lyricBlurStep: CGFloat = 0.9
    static let lyricBlurMax: CGFloat = 5.5
    /// `--lyric-glow`: two stacked glows round a held last word's letters, radii as shares of the line's height.
    static let lyricGlowTight: CGFloat = 0.025
    static let lyricGlowTightAlpha = 0.7
    static let lyricGlowWide: CGFloat = 0.05
    static let lyricGlowWideAlpha = 0.38
    struct LetterGlow: Sendable { let color: RGBA; let radius: CGFloat }
    static let lyricGlow = [LetterGlow(color: RGBA(1, 1, 1, lyricGlowTightAlpha), radius: lyricGlowTight),
                            LetterGlow(color: RGBA(1, 1, 1, lyricGlowWideAlpha), radius: lyricGlowWide)]
    /// `--lyric-fill-pace`: characters a second, never slower than the time to the next line allows.
    nonisolated static let lyricFillPace = 14.0
    /// `--lyric-hold-min`: seconds a pause or held last word needs to get the glow.
    nonisolated static let lyricHoldMin = 0.5
    /// `--lyric-glow-fade`, in seconds.
    nonisolated static let lyricGlowFade = 0.3
    static let lyricUnsung = RGBA.css(255, 255, 255, 0.58)
    /// `--lyric-move`, `--lyric-ease`, `--lyric-stagger`: seconds, and each lower line's delay after the one above.
    nonisolated static let lyricMove = 0.62
    /// `--lyric-move-lead`: seconds early, so the move has landed when the fill begins.
    nonisolated static let lyricMoveLead = lyricMove
    /// `--lyric-move-settle`: seconds the spring runs before it's at rest.
    nonisolated static let lyricMoveSettle = 1.4
    /// `--lyric-bounce`: how much a lyric line overshoots and settles when the lines move up.
    nonisolated static let lyricBounce = 0.35
    /// the shortest wait before a lyric line starts moving (one frame).
    nonisolated static let lyricMoveDelayMin = 1.0 / 120
    /// `--lyric-backing-scale`, `--lyric-backing-gap`: bracketed backing vocals on their own line under it.
    static let lyricBackingScale: CGFloat = 0.62
    static var lyricBackingGap: CGFloat { px(4) }
    /// `--lyric-gap-min`: seconds without lyrics that get the gap marker.
    nonisolated static let lyricGapMin = 5.0
    /// `--lyric-line-max`: the longest a line counts as sung, in seconds, when the server gives no end.
    nonisolated static let lyricLineMax = 6.0
    /// the gap marker's bars (`--lyric-gap-*`), sizes as shares of the type size.
    static let lyricGapBars = 4
    static let lyricGapWidth: CGFloat = 0.23
    static let lyricGapBarGap: CGFloat = 0.18
    static let lyricGapBarMin: CGFloat = 0.23
    static let lyricGapHeight: CGFloat = 1.14
    static let lyricGapBarStill = 0.5
    static let lyricGapUnlit = RGBA.css(255, 255, 255, 0.24)
    static let lyricGapGlow: CGFloat = 2.5
    /// `--lyric-gap-bounce-*`: bar i loops at `rate + i * spread` radians a second, offset by `i * phase`.
    static let lyricGapBounceRate = 2.1
    static let lyricGapBounceSpread = 0.37
    static let lyricGapBouncePhase = 1.9
    /// `--lyric-gap-swell`, `--lyric-gap-pop`: seconds the bars swell, then pop, ending `lyricGapPopLead` before the next line.
    static let lyricGapSwell = 0.65
    static let lyricGapPop = 0.14
    static let lyricGapPopLead = 0.05
    static let lyricGapSwellGrow = 0.35
    static let lyricGapSwellGlow = 0.6
    nonisolated static let lyricEase: (Double, Double, Double, Double) = (0.3, 1.18, 0.4, 1)
    nonisolated static let lyricStagger = 0.03
    /// how many lines below the current one the stagger reaches; lines further down move with the last of them.
    nonisolated static let lyricStaggerLines = 8
    /// while the window is dragged to a new size, how far the current lyric line may drift before the view follows it.
    static var lyricResizeDrift: CGFloat { px(24) }
    /// `--lyric-word-lift` (a share of the type size), `--lyric-word-lift-time`, `--lyric-word-lift-ease`, `--lyric-word-fall`.
    nonisolated static let lyricWordLift: CGFloat = 0.09
    nonisolated static let lyricWordLiftTime = 1.5
    /// `--lyric-word-lift-lead`: seconds a word starts rising before the fill reaches it.
    nonisolated static let lyricWordLiftLead = 0.3
    nonisolated static let lyricWordLiftEase: (Double, Double, Double, Double) = (0.25, 0.1, 0.25, 1)
    nonisolated static let lyricWordFall = 0.7
    /// `--lyric-lift-bounce`, `--lyric-lift-settle`: the dip below the line as a share of the lift, and its seconds.
    nonisolated static let lyricLiftBounce: CGFloat = 0.18
    nonisolated static let lyricLiftSettle = 0.32
    /// `--lyric-wave-*`: the held last word's rolling letters; height a share of the type size, phase a share of a cycle per
    /// letter, in and out in seconds.
    nonisolated static let lyricWaveHeight = 0.024
    nonisolated static let lyricWavePeriod = 0.643
    nonisolated static let lyricWavePhase = 0.16
    nonisolated static let lyricWaveIn = 0.25
    nonisolated static let lyricWaveOut = 0.45
    /// seconds before Core Animation layers take over a line's fill, once it has sharpened, so the swap can't be seen.
    nonisolated static let lyricLayerSettle = 0.36
    /// how many values a second the layers' animations are worked out at.
    nonisolated static let lyricLayerRate = 60.0
    /// the layers' clock is put right once it's this many seconds off the song's.
    nonisolated static let lyricLayerDrift = 0.03
    nonisolated static let lyricWordSoftEdge: CGFloat = 0.8
    /// the full screen player's right column (`--expanded-*`).
    static var expandedPanelTop: CGFloat { px(88) }
    static var expandedEdge: CGFloat { px(24) }
    static var expandedTopRow: CGFloat { px(24) }
    /// `--lyric-expanded`: CSS `clamp(28px, 2.7vw, 44px)` of the window's CSS width.
    static let lyricExpandedShare: CGFloat = 0.027
    static let lyricExpandedMin: CGFloat = 28
    static let lyricExpandedMax: CGFloat = 44
    /// `--lyric-plain-scale`, `--lyric-plain`, `--lyric-plain-gap`: unsynced lyrics in the full screen player, as a share of `--lyric-expanded`, their ink and their line gap.
    static let lyricPlainScale: CGFloat = 1.0
    static let lyricPlain = 0.85
    static let lyricPlainGap: CGFloat = 18
    /// room beside the full-screen lyric column so the gap marker's glow and the line blur aren't cut.
    static let lyricBlurRoom: CGFloat = 72
    /// "Written By" after the lyrics (`--lyrics-credit-*`): its size and ink as shares of the lyrics'.
    static let lyricsCreditScale: CGFloat = 0.5
    static let lyricsCreditOpacity = 0.6
    /// `--cover-paused-scale`: the full screen cover while paused.
    static let coverPausedScale: CGFloat = 0.73
    nonisolated static let coverPause = 0.5
    /// `--player-meta-gap`, `--player-transport-gap`, `--player-volume-gap`: the player's title rows to the seek line,
    /// the seek line to the transport, and the transport to the volume row.
    static var playerMetaGap: CGFloat { px(16) }
    static var playerTransportGap: CGFloat { px(12) }
    static var playerVolumeGap: CGFloat { px(8) }
    /// `--marquee-*` (`MarqueeText`): speed in points a second, pause in seconds.
    static var marqueeSpeed: CGFloat { px(30) }
    static let marqueePause = 3.0
    static var marqueeFade: CGFloat { px(18) }
    static var marqueeGap: CGFloat { px(48) }
    /// how long the leading edge's fade takes to come in once a roll starts.
    static let marqueeFadeIn = 0.15
    /// the truncated line under a running marquee: unseen, but not 0, so its links still take clicks.
    static let marqueeRestOpacity = 0.001
    /// `--sidebar-active-weight`: the current page's label in the sidebar.
    static let sidebarActiveWeight = Font.Weight.semibold
    /// `--bar-scrub-rest`, `--bar-scrub-hover`: the bar's progress line at rest and under the pointer.
    static var barScrubRest: CGFloat { px(2) }
    static var barScrubHover: CGFloat { px(5) }
    /// `--bar-scrub-rise`, `--bar-rows-blur`, `--bar-rows-dim`: the line's rise on hover and the title rows behind the times.
    static var barScrubRise: CGFloat { px(6) }
    static var barRowsBlur: CGFloat { px(5) }
    static let barRowsDim = 0.45
    /// `--bar-art-expand`: the glyph over the bar's artwork on hover; `--bar-scrub-reach` keeps the thin line easy to hit.
    static var barArtExpand: CGFloat { px(22) }
    /// `--bar-art`: the bar's artwork.
    static var barArt: CGFloat { px(38) }
    /// `--bar-art-inset`: the artwork's distance from the bar's top and leading edges, the same on both.
    static var barArtInset: CGFloat { px(11) }
    /// `--bar-line-gap`: between the artwork and the progress line under it.
    static var barLineGap: CGFloat { px(5) }
    /// `--bar-art-grow`: how much the bar's artwork grows under the pointer.
    static let barArtGrow: CGFloat = 1.08
    /// `--bar-scrub-delay`: how long the pointer rests on the bar's line before it rises and the times show.
    static let barScrubDelay = 0.35
    static var barScrubReach: CGFloat { px(8) }
    static var sidebarBlur: CGFloat { px(38) }
    static var edgeBlur: CGFloat { px(24) }
    /// Soft copies of artwork: the dock's reflection, the hero wash, the page glow and the player's backdrop.
    static var reflectBlur: CGFloat { px(32) }
    /// The reflection's opacity and the soft copies' saturation.
    static let reflectOpacity = 0.52
    static let reflectSaturation = 1.2
    /// An artist's name over the glow or a cover.
    static let heroTitleShadow = Shadow(color: RGBA(0, 0, 0, 0.4), blur: 22, y: 2)
    /// The hero wash behind Discover.
    static var heroGlowBlur: CGFloat { px(44) }
    static let heroGlowSaturation = 1.3
    static let heroGlowOpacity = 0.6
    /// The page glow and the expanded player's glow. Brightness multiplies, as CSS `brightness()` does.
    static var pageGlowBlur: CGFloat { px(90) }
    static let pageGlowSaturation = 1.2
    static let pageGlowBrightness = 0.72
    static let pageGlowOpacity = 0.72
    static var playerGlowBlur: CGFloat { px(110) }
    static let playerGlowSaturation = 1.3
    static let playerGlowBrightness = 0.82
    static let playerGlowOpacity = 0.55
    /// The bar glow's strength at its brightest (`--bar-glow-strength`).
    static let barGlowStrength = 0.24

    /// The system blur behind a translucent window, kept dark by the app's forced dark appearance. `.sidebar` is what
    /// Finder's sidebar uses, for its strong, soft blur of the desktop.
    static let windowMaterial = NSVisualEffectView.Material.sidebar
    /// The page color over that blur: secondary text keeps 4:1 contrast even over pure white.
    static let windowTint = bg.opacity(0.74)
    /// The window server's blur radius behind the translucent window, in its own units.
    static let windowBlurRadius = 80

    /// A CSS box shadow: `blur` is the CSS blur length in tokens (SwiftUI's radius is half) and `y` the offset.
    /// SwiftUI has no spread, so negative spreads are left out.
    struct Shadow {
        var color: RGBA
        var blur: CGFloat
        var x: CGFloat = 0
        var y: CGFloat = 0
    }
    /// Floating panels, dialogs, the bar and the sidebar drawer.
    static let shadowFloat = Shadow(color: RGBA(0, 0, 0, 0.7), blur: 50, y: 18)
    /// The top bar's glass pills.
    static let shadowPill = Shadow(color: RGBA(0, 0, 0, 0.5), blur: 26, y: 8)
    /// A page's big cover.
    static let shadowArt = Shadow(color: RGBA(0, 0, 0, 0.7), blur: 48, y: 20)
    /// A row lifted by a drag, and the dock over the page.
    static let shadowLift = Shadow(color: RGBA(0, 0, 0, 0.45), blur: 30, y: 12)
    static let shadowDock = Shadow(color: RGBA(0, 0, 0, 0.31), blur: 22, x: -11)
    /// The expanded player's cover.
    static let shadowPlayerArt = Shadow(color: RGBA(0, 0, 0, 0.6), blur: 70, y: 30)
    /// The count badge on a multi-row drag.
    static let shadowBadge = Shadow(color: RGBA(0, 0, 0, 0.35), blur: 12, y: 4)
    /// A sidebar row being dragged, slider thumbs, and the EQ graph's handles.
    static let shadowGhost = Shadow(color: RGBA(0, 0, 0, 0.4), blur: 18, y: 6)
    static let shadowKnob = Shadow(color: RGBA(0, 0, 0, 0.4), blur: 4, y: 1)
    static let shadowKnobHue = Shadow(color: RGBA(0, 0, 0, 0.5), blur: 5, y: 1)
    static let shadowEqHandle = Shadow(color: RGBA(0, 0, 0, 0.45), blur: 8, y: 2)
    /// The play glyph over a thumbnail.
    static let hoverPlayShadow = Shadow(color: RGBA(0, 0, 0, 0.6), blur: 4, y: 1)

    /// Drag feedback: the empty slot where rows will land, and the lifted card with a second one peeking under it.
    /// The table's AppKit card lets a little of the rows show through.
    static let dragSlot = RGBA.css(255, 255, 255, 0.05)
    static let dragSlotEdge = RGBA.css(255, 255, 255, 0.07)
    static let dragCard = panel2.opacity(0.9)
    static let dragCardEdge = RGBA.css(255, 255, 255, 0.12)
    static let dragCardUnder = RGBA(hex: "34343a")!.opacity(0.86)
    static let dragCardUnderEdge = RGBA.css(255, 255, 255, 0.08)
    /// Songs dragged out of their list: the card that follows the pointer, how dim the rows left behind go, and the
    /// drop target's accent ring, `dropPad` past a target without its own box.
    static let dragOutCardW = px(280)
    /// The card's offset below and right of the pointer.
    static let dragOutOffsetX = px(14)
    static let dragOutOffsetY = px(12)
    static let dragOutDim = 0.4
    static let rowUnavailableOpacity = 0.4
    static let dropRing = px(2)
    static let dropPadCard = px(8)

    /// The EQ graph: grid lines, the 0 dB line, a handle's ring and the curve's area.
    static let eqGrid = RGBA.css(255, 255, 255, 0.09)
    static let eqGridBand = RGBA.css(255, 255, 255, 0.04)
    static let eqGridZero = RGBA.css(255, 255, 255, 0.26)
    static let eqHandleRing = RGBA(0, 0, 0, 0.25)
    static let eqAreaOpacity = 0.14
    /// Status boxes, problem fills and icons, and the thin ring round an invalid field or rule value.
    static let statusFill = RGBA.css(255, 255, 255, 0.06)
    static let dangerFill = danger.opacity(0.14)
    static let dangerInk = danger.mixed(with: RGBA(1, 1, 1), amount: 0.7)
    static let invalidRing = RGBA.css(255, 120, 120, 0.7)
    /// `--invalid-outline`'s width: a thin ring, so a value with a problem is marked quietly where it is.
    static let invalidRingW: CGFloat = 1
    static var statusPadY: CGFloat { px(11) }
    static var statusPadX: CGFloat { px(14) }
    static var statusGap: CGFloat { px(12) }
    /// A one-time question inside a panel (`PromptCard`): its padding and the gap between its parts.
    static var promptCardPad: CGFloat { px(16) }
    static var promptCardGap: CGFloat { px(10) }

    /// The brand: the dark app icon's gradient behind the sign-in screen, and the lockup of the mark and "navia" in
    /// Mona Sans 750 / width 125. Sizes are fractions of the mark's ink and match GPUI's `theme.rs`.
    static let brandTop = RGBA(hex: "3a3a40")!
    static let brandBottom = RGBA(hex: "0b0b0d")!
    static let brandWordWeight: CGFloat = 750
    static let brandWordWidth: CGFloat = 125
    static let brandWordTracking: CGFloat = -0.015
    nonisolated static let brandMarkStroke: CGFloat = 4.4
    static let brandWordRatio: CGFloat = 1.1
    static let brandGapRatio: CGFloat = 0.3

    /// The sign-in screen's form, card and lockup, and a labeled field's gaps.
    static var connectW: CGFloat { px(420) }
    static var connectGap: CGFloat { px(16) }
    static var connectHeadingGap: CGFloat { px(8) }
    static var connectCardPad: CGFloat { px(28) }
    static var connectMark: CGFloat { px(55) }
    static var fieldLabelGap: CGFloat { px(7) }
    static var fieldMessageGap: CGFloat { px(7) }
    static var fieldInset: CGFloat { px(3) }

    /// `--explicit-fill`, `--explicit-ink`: the explicit "E" mark.
    static let explicitFill = RGBA.css(255, 255, 255, 0.45)
    static let explicitInk = RGBA(0, 0, 0, 0.82)
    /// The tint when an album has none.
    static let tintFallback = RGBA(hex: "34506b")!

    /// Timing. Every hover change takes `hoverIn` going on and `hoverOut` going off, and a press `hoverPress`, all on
    /// the `hoverEase` cubic-bezier. The same values as GPUI.
    nonisolated static let hoverIn = 0.1
    nonisolated static let hoverOut = 0.24
    nonisolated static let hoverPress = 0.1
    nonisolated static let hoverEase: (Double, Double, Double, Double) = (0.2, 0.7, 0.2, 1)
    /// Controls an area's hover reveals stay this long after the pointer leaves, then fade over `hoverOut`.
    nonisolated static let revealGrace = 0.3
    nonisolated static let fast = 0.18
    nonisolated static let med = 0.34
    nonisolated static let dragSlide = 0.17
    nonisolated static let tintFade = 0.32
    nonisolated static let toggleFade = 0.2
    nonisolated static let playerEnter = 0.38
    nonisolated static let shelfScroll = 0.3
    /// `--panel-slide`: the sidebar, side player and bar sliding in and out while the page resizes with them.
    nonisolated static let panelSlide = 0.3
    /// seconds at most that a panel coming in waits, built off-screen, for the app to keep up with the display again.
    nonisolated static let panelBuildWait = 0.3
    /// a panel waiting to slide in: above 0, so it's drawn, and too faint to see.
    nonisolated static let panelWarmOpacity = 0.002
    /// `--art-fade`: artwork fading in as it arrives.
    nonisolated static let artFade = 0.18
    /// `--song-fade`: the player's artwork and title cross-fading on a song change.
    nonisolated static let songFade = 0.25
    /// `--view-fade`, `--view-rise`: the player's views cross-fading, the incoming one rising.
    nonisolated static let viewFade = 0.22
    static var viewRise: CGFloat { px(8) }
    /// `--page-fade`, `--page-rise`: a new page fading in rising, or falling after Back.
    nonisolated static let pageFade = 0.15
    static var pageRise: CGFloat { px(6) }
    /// `--land-fade`, `--land-stagger`: a page's sections fading in as their content arrives, one after another; those
    /// past `landStaggerMax` share its delay.
    nonisolated static let landFade = 0.24
    nonisolated static let landStagger = 0.04
    nonisolated static let landStaggerMax = 4
    /// `--like-pop`, `--like-pop-scale`, `--like-pop-peak`: the heart growing and back as a song is liked, largest at that share of the time.
    nonisolated static let likePop = 0.3
    nonisolated static let likePopScale: CGFloat = 1.28
    nonisolated static let likePopPeak = 0.4
    /// `--row-flash`: rows lit as songs are queued or added to a playlist, fading back.
    nonisolated static let rowFlash = 0.35
    /// every animation is nil (instant) while motion is reduced, so views never check the setting.
    nonisolated static func ease(_ duration: Double) -> Animation? { Motion.isReduced ? nil : .timingCurve(0.2, 0.7, 0.2, 1, duration: duration) }
    nonisolated static func easeOut(_ duration: Double) -> Animation? { Motion.isReduced ? nil : .easeOut(duration: duration) }
    nonisolated static func easeInOut(_ duration: Double) -> Animation? { Motion.isReduced ? nil : .easeInOut(duration: duration) }
    nonisolated static func hover(_ on: Bool) -> Animation? { hoverCurve(on ? hoverIn : hoverOut) }
    /// Controls an area's hover reveals: in like a hover, out after `revealGrace`.
    nonisolated static func reveal(_ on: Bool) -> Animation? { on ? hover(true) : hover(false)?.delay(revealGrace) }
    /// A press going down (`hoverPress`), or coming back up (`hoverIn`).
    nonisolated static func press(_ down: Bool) -> Animation? { hoverCurve(down ? hoverPress : hoverIn) }
    /// seconds per `PressPhase` step: `pressIn` going down, `pressOut` going up.
    nonisolated static let pressIn = 0.09
    nonisolated static let pressOut = 0.12
    nonisolated static let pressSwap = 0.1
    nonisolated static func activeFill(_ held: Bool) -> Animation? { easeOut(held ? activeFillGrow : activeFillFade) }
    nonisolated static func hoverCurve(_ duration: Double) -> Animation? {
        Motion.isReduced ? nil : .timingCurve(hoverEase.0, hoverEase.1, hoverEase.2, hoverEase.3, duration: duration)
    }
    /// A toggle's on state (shuffle, repeat, a switcher's segment, a view button) easing in and out, `toggleFade` both ways.
    nonisolated static var toggle: Animation? { hoverCurve(toggleFade) }
    nonisolated static let lyricWordLiftCurve = UnitCurve.bezier(startControlPoint: UnitPoint(x: lyricWordLiftEase.0, y: lyricWordLiftEase.1),
                                                                 endControlPoint: UnitPoint(x: lyricWordLiftEase.2, y: lyricWordLiftEase.3))
    /// the expanded player's cover settling to its playing or paused size, on the lyrics' rising-then-settling curve.
    nonisolated static var coverScale: Animation? {
        Motion.isReduced ? nil : .timingCurve(lyricEase.0, lyricEase.1, lyricEase.2, lyricEase.3, duration: coverPause)
    }
    /// AppKit's animations, eased out, or at once while motion is reduced.
    static func animate(_ duration: Double, _ changes: () -> Void) {
        NSAnimationContext.runAnimationGroup { context in
            context.duration = Motion.isReduced ? 0 : duration
            context.timingFunction = CAMediaTimingFunction(name: .easeOut)
            changes()
        }
    }
    /// AppKit's own animations, such as the table opening a gap under a drag: instant while motion is reduced.
    static func appKitMotion<R>(_ body: () -> R) -> R {
        guard Motion.isReduced else { return body() }
        NSAnimationContext.beginGrouping()
        NSAnimationContext.current.duration = 0
        NSAnimationContext.current.allowsImplicitAnimation = false
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        defer {
            CATransaction.commit()
            NSAnimationContext.endGrouping()
        }
        return body()
    }
    /// Whether a canceled table drag slides its card back to the rows.
    static var dragSlidesBack: Bool { !Motion.isReduced }
    /// A spinner's clock. It stands still while motion is reduced.
    static var spinSchedule: AnimationTimelineSchedule { .animation(paused: Motion.shared.reduced) }
    /// One turn of the busy glyph, in seconds.
    static let spinPeriod: Double = 0.9

    static var textXS: CGFloat { px(14) }
    static var textSM: CGFloat { px(15) }
    static var textMD: CGFloat { px(16) }
    static var textLG: CGFloat { px(18) }
    static var textSection: CGFloat { px(25) }
    static var textFeature: CGFloat { px(21) }
    static var textSubtitle: CGFloat { px(23) }
    static var textTitle: CGFloat { px(42) }

    static var rSM: CGFloat { px(8) }
    static var rMD: CGFloat { px(12) }
    static var rRow: CGFloat { px(13) }
    static var rLG: CGFloat { px(22) }

    /// The page's side margin. A page inside the split view reads its gutter from `PageContext.gutter`.
    static var gutter: CGFloat { px(45) }
    /// The split view (Albums, Artists): the list, its rows and the page beside it. Below `splitMinW` (CSS width)
    /// only the page shows.
    static var splitListW: CGFloat { px(315) }
    static var splitListPad: CGFloat { px(20) }
    static var splitRowH: CGFloat { px(78) }
    static var splitRowPad: CGFloat { px(5) }
    static var splitArt: CGFloat { px(45) }
    static var splitRowGap: CGFloat { px(13) }
    static var splitRowInset: CGFloat { px(6) }
    static var splitLineFrom: CGFloat { px(60) }
    static var splitGlyph: CGFloat { px(26) }
    static var splitGutter: CGFloat { px(20) }
    static var splitEnd: CGFloat { px(25) }
    static let splitMinW: CGFloat = 760
    static var contentTop: CGFloat { px(32) }
    /// `--sidebar-w`, `--sidebar-w-min`, `--sidebar-w-max`, unscaled tokens so a saved width survives an Interface size change.
    static let sidebarW: CGFloat = 245
    static let sidebarWMin: CGFloat = 200
    static let sidebarWMax: CGFloat = 360
    /// `--sidebar-grab-w` and the dock's twin: the invisible strip straddling a resizable edge.
    static var sidebarGrabW: CGFloat { px(12) }
    static var dockGrabW: CGFloat { sidebarGrabW }
    /// `--dock-w` and its drag range, unscaled tokens like the sidebar's.
    static let dockW: CGFloat = 437
    static let dockMinW: CGFloat = 340
    static let dockMaxW: CGFloat = 640
    /// What the page keeps beside a dragged dock: the room the default dock leaves at the narrowest docked window.
    static var pageMinW: CGFloat { px(478) }
    static var shelfFade: CGFloat { px(130) }
    static var shelfGap: CGFloat { px(4) }
    /// The margin the sidebar, the dock and the bottom bar keep from the window's edges.
    static var windowInset: CGFloat { px(10) }
    static var windowControlsW: CGFloat { px(76) }
    static var barW: CGFloat { px(1010) }
    static var barH: CGFloat { px(62) }
    /// The player chip over the bar, centered above the play controls.
    static var chipAboveMaxW: CGFloat { px(360) }
    static var chipAboveGap: CGFloat { px(10) }
    /// A code entry such as the AirPlay PIN: digit boxes, the gap between them, and the digit size.
    static var pinBoxW: CGFloat { px(54) }
    static var pinBoxH: CGFloat { px(62) }
    static var pinGap: CGFloat { px(10) }
    static var pinText: CGFloat { textSection }
    /// The AirPlay output badge: glyph to name, and the name's max width beside the bar's title.
    static var outputBadgeGap: CGFloat { px(6) }
    static var outputBadgeMaxW: CGFloat { px(200) }
    /// `--quality-glyph`, `--quality-glyph-twin-gap`: the quality badge's lossless wave, as parts of the badge text; hi-res stacks the same wave twice, centre to centre this far apart.
    static let qualityGlyph: CGFloat = 0.905
    static let qualityGlyphTwinGap: CGFloat = 0.419
    static var topbarH: CGFloat { px(76) }
    static var pillH: CGFloat { px(56) }
    static var controlH: CGFloat { px(40) }
    static var iconW: CGFloat { px(38) }
    static var iconButtonH: CGFloat { px(34) }
    static var actionW: CGFloat { px(41) }
    /// song table number columns: Time and Plays, right-aligned with `numCellPad` after them; the song name ends with `nameCellPad`, other cells with `cellPad`.
    static var timeColW: CGFloat { px(78) }
    static var playsColW: CGFloat { px(76) }
    static var numCellPad: CGFloat { px(20) }
    static var cellPad: CGFloat { px(14) }
    static var nameCellPad: CGFloat { px(12) }
    /// `--popularity-col-w`, `--popularity-bar-w`, `--popularity-bar-gap`, `--popularity-bar-h`: the Heat meter, drawn on whole device pixels.
    static var popularityColW: CGFloat { px(64) }
    static var popularityBarW: CGFloat { px(2) }
    static var popularityBarGap: CGFloat { px(2) }
    static var popularityBarH: CGFloat { px(14) }
    /// `--popularity-on`, `--popularity-off` (`--t1` at 34%), `--popularity-heat-foot`, `--popularity-heat-top`; the playing row's lit bars are the accent's soft step.
    static var popularityOn: RGBA { t1 }
    static var popularityOff: RGBA { t1.opacity(t1.a * 0.34) }
    static var popularityHeatFoot: RGBA { t2 }
    static let popularityHeatTop = RGBA.css(255, 255, 255, 1)
    /// `--popularity-heat-sweep`, `--popularity-heat-sweep-step` (seconds): a light passes bar to bar once a period, rising over the first `heatFlashPeak` of it and gone by `heatFlashEnd`.
    static let popularityHeatSweep: Double = 4
    static let popularityHeatSweepStep: Double = 0.09
    static let heatFlashPeak: Double = 0.05
    static let heatFlashEnd: Double = 0.12
    nonisolated static let ease: (Double, Double, Double, Double) = (0.2, 0.7, 0.2, 1)
    static var thumb: CGFloat { px(42) }
    /// A list page (Downloads, Share Links): max width, gaps and row padding.
    static var listPageW: CGFloat { px(980) }
    static var listPageGap: CGFloat { px(30) }
    static var listRowPadX: CGFloat { px(10) }
    /// A list page's song or collection row.
    static var listRowH: CGFloat { px(68) }
    /// Share links: cover to title, the end-date and visits columns, and the button gap.
    static var shareSubjectGap: CGFloat { px(12) }
    static var shareEndsW: CGFloat { px(170) }
    static var shareVisitsW: CGFloat { px(170) }
    static var shareActionsGap: CGFloat { px(2) }
    static var cardW: CGFloat { px(200) }
    static var cardGap: CGFloat { px(32) }
    static var tileGap: CGFloat { px(20) }
    /// the narrowest card in a grid page; the cards stretch from it to fill the width (`gridFit`).
    static var gridCard: CGFloat { px(224) }
    static var gridRowGap: CGFloat { px(34) }
    static var heroArt: CGFloat { px(250) }
    /// The artist header: the photo at most `artistArtMaxRatio` as wide as tall, the gaps around the name, and the
    /// Top Songs and Latest Release columns under it.
    static let artistArtMaxRatio: CGFloat = 1.6
    static var artistHeadGap: CGFloat { px(32) }
    static var artistHeadTitleGap: CGFloat { px(16) }
    static var artistColumnsGap: CGFloat { px(44) }
    static var artistColumnsStackGap: CGFloat { px(24) }
    static var sectionGap: CGFloat { px(42) }
    /// A section heading's gap above its rows or cards, and to a button at its right.
    static var shelfHeadGap: CGFloat { px(16) }
    /// How far below the visible rows a later section counts as near view and gets loaded. Matches GPUI's
    /// `NEAR_VIEW_MARGIN`.
    static var nearViewMargin: CGFloat { 420 }
    /// The gap below a playlist's sort note. The table follows the note directly.
    static var orderNoteGap: CGFloat { px(28) }
    static var miniRowH: CGFloat { px(65) }
    /// The mini player: its compact size and large square, padding, controls, glass, scrim, and starting margin from
    /// the screen corner.
    static var miniW: CGFloat { px(366) }
    static var miniH: CGFloat { px(100) }
    static var miniLarge: CGFloat { px(366) } // = miniW: the large one is a true square.
    static var miniR: CGFloat { rFloat }
    static var miniPad: CGFloat { px(12) }
    static var miniGap: CGFloat { px(12) }
    static var miniButton: CGFloat { px(34) }
    static var miniChromeButton: CGFloat { px(28) }
    static let miniGlass = 0.86
    static let miniScrimStart = 0.34
    static let miniScrimStrength = 0.92
    static var miniCornerMargin: CGFloat { px(16) }
    /// It resizes between the compact size and `miniMax` square, and stacks (cover behind, body over a scrim) once it's
    /// at least `miniStackH` tall and `miniStackRatio` of its width.
    static var miniMinW: CGFloat { miniW }
    static var miniMinH: CGFloat { miniH }
    static var miniMax: CGFloat { px(640) }
    static var miniStackH: CGFloat { px(220) }
    static let miniStackRatio: CGFloat = 0.6
    /// The band along each edge that resizes it. Corners take squares twice as big.
    static var miniGrip: CGFloat { px(8) }
    /// The mini player's hairline seek bar, and a sync's progress bar and meter width. The sync track is the download
    /// ring's track.
    static var syncBarH: CGFloat { px(3) }
    static var syncMeterW: CGFloat { px(280) }
    static var syncTrack: RGBA { downloadTrack }
    /// With no total, a segment sweeps across the sync bar. A sync that isn't the first shows only after
    /// `syncShowAfter`.
    static let syncSweep: Double = 1.4
    static let syncSweepSpan: Double = 0.3
    static let syncShowAfter: Double = 10
    static var scrubThin: CGFloat { px(3) }
    static var scrubThinHover: CGFloat { px(5) }
    /// `--scrub-line`, `--scrub-line-hover`: the dock's and full screen's seek line, and how thick it gets under the pointer.
    static var scrubLine: CGFloat { px(4) }
    /// `--scrub-slot`: the height a seek line takes in layout, more than the line itself so it's easy to grab.
    static var scrubSlot: CGFloat { px(14) }
    static var scrubLineHover: CGFloat { px(7) }
    static var scrubChipEdge: CGFloat { px(46) }
    static var rXS: CGFloat { px(6) }
    /// How far a card or tile rises under the pointer.
    static var albumHeadGap: CGFloat { px(25) }
    static var albumInfoPadTop: CGFloat { px(20) }
    static var albumArtistGap: CGFloat { px(14) }
    static var albumMetaGap: CGFloat { px(12) }
    static var albumSectionGap: CGFloat { px(32) }
    /// `--countdown-*`; `countdownPadX` once it says "Out now".
    static var countdownR: CGFloat { rFloat }
    static var countdownGap: CGFloat { px(20) }
    static var countdownPadY: CGFloat { px(14) }
    static var countdownPadX: CGFloat { px(28) }
    static var countdownCellW: CGFloat { px(92) }
    static var countdownNumber: CGFloat { px(34) }
    static var countdownLabel: CGFloat { textXS }
    static var countdownLabelGap: CGFloat { px(4) }
    static let countdownFill = RGBA(0, 0, 0, 0.24)
    static let countdownDivider = glassDivider
    /// `--cover-label-inset`, `--cover-label-pad-y`, `--cover-label-pad-x`: the quiet "Mix" chip on a cover the app made.
    static var coverLabelInset: CGFloat { px(8) }
    static var coverLabelPadY: CGFloat { px(2) }
    static var coverLabelPadX: CGFloat { px(8) }
    /// `--cover-mark-*`: the bare label over a photo, as shares of the cover's width; tracking in em.
    static let coverMarkInset: CGFloat = 0.057
    static let coverMarkText: CGFloat = 0.052
    static let coverMarkGlyph: CGFloat = 0.076
    static let coverMarkGap: CGFloat = 0.029
    static let coverMarkTracking: CGFloat = 0.08
    /// `--art-glyph`: missing artwork's stand-in glyph, its point size as a share of the artwork.
    static let artGlyph: CGFloat = 0.406
    static let coverMarkInk = RGBA(1, 1, 1, 0.75)
    /// `--card-text-gap`: from a card's art (or tile) to the text under it.
    static var cardTextGap: CGFloat { px(10) }
    /// Artist Radio tiles (`--station-*`), as shares of the tile's width. the shade runs from the tint mixed toward black
    /// (`stationShade`) at the bottom to clear at `stationShadeEnd` of the height; `scrim` behind the mark clears at
    /// `stationMarkShadeEnd`. `stationInset` is the photo's hover inset and corner radius.
    static let stationPad: CGFloat = 0.057
    static let stationName: CGFloat = 0.10
    static let stationNameBottom: CGFloat = 0.052
    static let stationFace: CGFloat = 0.133
    static let stationFaceGap: CGFloat = 0.019
    static let stationFacesBottom: CGFloat = 0.19
    static let stationRing: CGFloat = 0.007
    static let stationRingInk = RGBA(1, 1, 1, 0.7)
    static let stationInset: CGFloat = 0.029
    static let stationFaceHover: CGFloat = 1.12
    static let stationFaceSpread: CGFloat = 0.014
    static let stationShade: Double = 0.88
    static let stationShadeEnd: CGFloat = 0.55
    static let stationMarkShadeEnd: CGFloat = 0.34

    /// genre tiles (`--genre-tile-ratio`, `--genre-fan-*`): up to three covers fanned out of the bottom right.
    static let genreTileRatio: CGFloat = 4.0 / 3.0
    /// The name's inset as fractions of the tile's width.
    static let genreTilePadY: CGFloat = 0.07
    static let genreTilePadX: CGFloat = 0.08
    /// A cover's width as a fraction of the tile's, and how far the front cover's corner sits past the tile's edges.
    static let genreFanSize: CGFloat = 0.46
    static let genreFanRight: CGFloat = -0.05
    static let genreFanBottom: CGFloat = -0.17
    /// One fanned cover: its shift as a fraction of its own size, and its turn. Front to back.
    struct FanCard: Sendable { let x: CGFloat; let y: CGFloat; let degrees: Double }
    static let genreFan = [FanCard(x: 0, y: 0, degrees: 10), FanCard(x: -0.30, y: 0, degrees: -2), FanCard(x: -0.56, y: 0, degrees: -14)]
    static let genreFanHover = [FanCard(x: 0, y: -0.03, degrees: 13), FanCard(x: -0.34, y: -0.02, degrees: -3), FanCard(x: -0.64, y: 0, degrees: -18)]
    static let genreFanSingle = FanCard(x: 0, y: 0, degrees: 6)
    /// A tinted tile darkens toward the covers' corner: the lightness multiplier there and the ramp's angle in CSS degrees.
    static let tileFillShade = 0.7
    static let tileFillAngle = 150.0
    /// The most lightness a tint background may have under light text.
    nonisolated static let tintTextSafeL = 0.47
    /// Dense genre rows: their minimum width, side padding, and cover-to-text gap.
    static var genreChipW: CGFloat { px(250) }
    static var genreRowPad: CGFloat { px(8) }
    static var genreRowGap: CGFloat { px(13) }

    /// 78 CSS pixels, 68.25 points. Whole points keep the 1px separators crisp.
    static var rowH: CGFloat { (68 * zoom).rounded() }
    static var rowCompactH: CGFloat { (38 * zoom).rounded() }

    /// Window widths below which the dock becomes the bar and the sidebar becomes a drawer.
    static var dockMinWindow: CGFloat { px(1180) }
    static var sidebarMinWindow: CGFloat { px(860) }
}

extension Font {
    static func navia(_ size: CGFloat, _ weight: Font.Weight = .regular) -> Font { .system(size: size, weight: weight) }
}

let accentPresets = ["5a93f5", "50ffff", "8b7cf6", "e879a6", "f2685c", "f5a33d", "4cc38a"]

/// colors the user can change at runtime (Settings > Accent color, saved as `accentHex`, or `accentSystem`).
@Observable final class Theme {
    static let defaultAccent = RGBA(hex: "5a93f5")!

    /// what everything draws with: the chosen colour, or the preset closest to the system's while System is chosen.
    private(set) var accent: RGBA
    private(set) var followsSystem: Bool
    @ObservationIgnored private var chosen: RGBA
    @ObservationIgnored private var systemWatch: NSObjectProtocol?

    init() {
        Self.migrateToSystemAccent()
        chosen = UserDefaults.standard.string(forKey: "accentHex").flatMap(RGBA.init(hex:)) ?? Theme.defaultAccent
        followsSystem = UserDefaults.standard.bool(forKey: "accentSystem")
        accent = chosen
        refresh()
        systemWatch = NotificationCenter.default.addObserver(forName: NSColor.systemColorsDidChangeNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.refresh() }
        }
    }

    /// `#rrggbb`, lower case.
    var hex: String {
        String(format: "#%02x%02x%02x", Int((accent.r * 255).rounded()), Int((accent.g * 255).rounded()), Int((accent.b * 255).rounded()))
    }

    func setAccent(hex: String) {
        guard let color = RGBA(hex: hex) else { return }
        choose(color)
    }

    /// Matches GPUI's `hue_to_hex`: a fixed saturation and lightness, so any hue works on the dark page.
    func setAccent(hue: Double) {
        choose(RGBA.hsl(hue, T.accentS, T.accentL))
    }

    /// one-release migration: every install moves to the System accent once, then later choices stick.
    private static func migrateToSystemAccent() {
        let defaults = UserDefaults.standard
        guard !defaults.bool(forKey: "accentSystemMigrated") else { return }
        defaults.set(true, forKey: "accentSystem")
        defaults.set(true, forKey: "accentSystemMigrated")
    }

    func followSystem() {
        followsSystem = true
        UserDefaults.standard.set(true, forKey: "accentSystem")
        refresh()
    }

    private func choose(_ color: RGBA) {
        chosen = color
        followsSystem = false
        UserDefaults.standard.set(false, forKey: "accentSystem")
        refresh()
        UserDefaults.standard.set(hex, forKey: "accentHex")
    }

    private func refresh() {
        let next = followsSystem ? Self.systemAccent() : chosen
        if next != accent { accent = next }
    }

    /// the system accent with its hue and chroma kept and its lightness held to the presets' range, so it reads on
    /// Navia's surfaces like they do; chroma drops only as far as sRGB needs. graphite and other greys give the default.
    static func systemAccent() -> RGBA {
        guard let system = NSColor.controlAccentColor.usingColorSpace(.sRGB) else { return defaultAccent }
        let color = RGBA(system.redComponent, system.greenComponent, system.blueComponent)
        let high = max(color.r, color.g, color.b), low = min(color.r, color.g, color.b), l = (high + low) / 2
        let saturation = high == low ? 0 : (high - low) / (1 - abs(2 * l - 1))
        guard saturation >= T.systemAccentMinS else { return defaultAccent }
        let lightness = accentPresets.compactMap(RGBA.init(hex:)).map { OKLCH.from($0).l }
        let o = OKLCH.from(color)
        let held = min(max(o.l, lightness.min() ?? o.l), lightness.max() ?? o.l)
        var chroma = o.c
        if !OKLCH.inSRGB(l: held, c: chroma, h: o.h) {
            var (fits, over) = (0.0, chroma)
            for _ in 0..<24 {
                let mid = (fits + over) / 2
                if OKLCH.inSRGB(l: held, c: mid, h: o.h) { fits = mid } else { over = mid }
            }
            chroma = fits
        }
        return OKLCH.rgb(l: held, c: chroma, h: o.h)
    }

    /// the accent's hsl hue, in degrees.
    var hue: Double {
        let (r, g, b) = (accent.r, accent.g, accent.b)
        let high = max(r, g, b), d = high - min(r, g, b)
        guard d > 0 else { return 0 }
        let h = high == r ? (g - b) / d : high == g ? (b - r) / d + 2 : (r - g) / d + 4
        return (h * 60 + 360).truncatingRemainder(dividingBy: 360)
    }

    /// `--row-selected`: a selected song row, the accent at 30%.
    var rowSelected: RGBA { Self.rowSelected(accent) }
    static func rowSelected(_ accent: RGBA) -> RGBA { accent.opacity(0.30) }
    /// `--row-flash-fill`: the accent at 24%.
    var rowFlashFill: RGBA { Self.rowFlashFill(accent) }
    static func rowFlashFill(_ accent: RGBA) -> RGBA { accent.opacity(0.24) }

    /// A downloaded collection's mark and a filling download ring.
    var downloaded: RGBA { accent }
    /// A sync's bar.
    var syncFill: RGBA { accent }
    var downloadedInk: RGBA { onAccent }

    /// A drop target under dragged songs: the accent at 24%.
    var dropTarget: RGBA { accent.opacity(0.24) }
    var dropTargetEdge: RGBA { accent }

    /// The current song's title.
    var accentSoft: RGBA { accent.mixed(with: RGBA(1, 1, 1), amount: 0.55) }
    /// `--link-ink`: a web address inside running text.
    var linkInk: RGBA { accentSoft }

    /// White or dark ink, whichever has the larger APCA contrast on the accent.
    var onAccent: RGBA {
        let dark = RGBA(hex: "0b1220")!, white = RGBA(1, 1, 1)
        return abs(white.apcaLc(on: accent)) > abs(dark.apcaLc(on: accent)) ? white : dark
    }
}

extension EnvironmentValues {
    /// True while drawing an offscreen check image, where AppKit-backed views can't render.
    @Entry var isSnapshot = false
    /// false while the main window isn't shown: its motion lands at once instead of animating unseen.
    @Entry var motionLive = true
    /// The user's colors, for shared views that aren't handed the shell (the sync meter under an empty state).
    @Entry var theme: Theme? = nil
}

/// Reduce Motion. Follows the system's Accessibility setting live, and the animation tokens in `T` read it.
/// Check runs can force it with `-naviaReduceMotion YES|NO`.
@Observable final class Motion {
    static let shared = Motion()
    /// Observed by views whose look depends on it (a spinner's clock).
    private(set) var reduced = false
    /// The same flag, readable from the animation tokens, which aren't on the main actor.
    nonisolated private static let flag = Atomic<Bool>(false)
    nonisolated static var isReduced: Bool { flag.load(ordering: .relaxed) }
    @ObservationIgnored private var forced: Bool?
    @ObservationIgnored private var observer: NSObjectProtocol?

    private init() {
        if UserDefaults.standard.object(forKey: "naviaReduceMotion") != nil { forced = UserDefaults.standard.bool(forKey: "naviaReduceMotion") }
        observer = NSWorkspace.shared.notificationCenter.addObserver(forName: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification,
                                                                      object: nil, queue: .main) { _ in
            MainActor.assumeIsolated { Motion.shared.update() }
        }
        update()
    }

    /// Check runs: force reduced motion on or off, or nil to follow the system again.
    func force(_ value: Bool?) {
        forced = value
        update()
    }

    private func update() {
        let on = forced ?? NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        Self.flag.store(on, ordering: .relaxed)
        if reduced != on { reduced = on }
    }
}

enum PressPhase {
    case rest, held, squash, over

    var scale: CGFloat {
        switch self {
        case .rest: 1
        case .held: T.pressHold
        case .squash: T.pressSquash
        case .over: T.pressOver
        }
    }

    var motion: Animation {
        switch self {
        case .held, .squash: .easeOut(duration: T.pressIn)
        case .over, .rest: .easeInOut(duration: T.pressOut)
        }
    }
}
