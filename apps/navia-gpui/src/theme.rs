//! The design tokens as a Rust theme. The Mac app's `Tokens.swift` uses the same names and values.
//!
//! Numbers are token units drawn at 87.5%, so `Theme::s` multiplies by 0.875 and by the user's
//! interface size. At 100%, `TEXT_MD` (16) is 14px.

use gpui::{Hsla, Pixels, Rgba, px, rgb, rgba};

use crate::color::{Srgb, apca_lc};

/// Screen pixels per token unit at 100% interface size.
pub const BASE_SCALE: f32 = 0.875;

// Colors that are the same in every theme.
pub const BG: u32 = 0x171718;
pub const PANEL: u32 = 0x1f1f21;
pub const PANEL_2: u32 = 0x2a2a2d;
/// `--active-fill`: behind a held player control (`Theme::active_fill`).
pub const ACTIVE_FILL: u32 = 0x00000038;
/// shown before the expanded player's backdrop is baked.
pub const EXPANDED_BG: u32 = 0x101014;
/// The danger color.
pub const DANGER: u32 = 0xe5484d;
/// The tint used when an album has none.
pub const TINT_FALLBACK: u32 = 0x34506b;
/// The shade the dock's gradient darkens toward at its foot. The player shades that follow are
/// the expanded player's diagonal gradient corners.
pub const DOCK_SHADE: u32 = 0x0a0e16;
pub const PLAYER_SHADE_START: u32 = 0x081018;
pub const PLAYER_SHADE_END: u32 = 0x060a14;
/// How much of the tint stays where the dock, mini player and expanded player darken toward a shade.
pub const PLAYER_SHADE_MIX: f32 = 0.72;
/// `--accent-wheel-check-shadow`: lifts the check off the System swatch's colour wheel; its colour and blur.
pub const ACCENT_WHEEL_CHECK_SHADOW: u32 = 0x000000d9;
pub const ACCENT_WHEEL_CHECK_BLUR: f32 = 2.0;
/// `--glass-edge`: the inset 1px ring and the faint top highlight of every glass surface.
pub const GLASS_EDGE: u32 = 0xffffff29;
pub const GLASS_EDGE_TOP: u32 = 0xffffff0f;
/// `--menu-edge-color`: menus and popovers take a fainter ring than other glass, with the same top highlight.
pub const MENU_EDGE: u32 = 0xffffff14;
/// `--button-edge`: a filled button's light hairline.
pub const BUTTON_EDGE: u32 = 0xffffff38;
/// The dark fade at the top of the dock, under its buttons.
pub const DOCK_HEAD_SHADE: u32 = 0x00000057;
/// `--window-button-ink`: a window button's glyph off the artwork.
pub const WINDOW_BUTTON_INK: u32 = 0xffffffd1;
/// `--window-button-on-art`, `--window-button-on-art-hover`: a window button's fill over artwork.
pub const WINDOW_BUTTON_ON_ART: u32 = 0x00000038;
pub const WINDOW_BUTTON_ON_ART_HOVER: u32 = 0x00000059;
/// a flat window button's hover fill on a panel, and the close button's under the pointer.
pub const WINDOW_BUTTON_HOVER: u32 = 0xffffff24;
pub const WINDOW_CLOSE: u32 = 0xda4453;
/// `--eq-grid`, `--eq-grid-band`, `--eq-grid-zero`, `--eq-handle-ring`, `--eq-handle-active-ring`.
pub const EQ_GRID: u32 = 0xffffff17;
pub const EQ_GRID_BAND: u32 = 0xffffff0a;
pub const EQ_GRID_ZERO: u32 = 0xffffff42;
pub const EQ_HANDLE_RING: u32 = 0x00000040;
pub const EQ_HANDLE_ACTIVE_RING: u32 = 0xffffff4d;
/// the fill under the EQ curve.
pub const EQ_AREA_OPACITY: f32 = 0.14;
/// `--status-fill`; `--danger-fill` and `--danger-ink` are `danger_fill` and `danger_ink`.
pub const STATUS_FILL: u32 = 0xffffff0f;
pub const DANGER_FILL_ALPHA: f32 = 0.14;
/// The ring around an invalid field.
pub const INVALID_RING: u32 = 0xff7878b3;
/// The invalid ring's width, thin so a problem is marked quietly in place.
pub const INVALID_RING_W: f32 = 1.0;
pub const DANGER_INK_MIX: f32 = 0.7;
/// The dark app icon's gradient, top to bottom, behind the sign-in screen and installer splash.
pub const BRAND_TOP: u32 = 0x3a3a40;
pub const BRAND_BOTTOM: u32 = 0x0b0b0d;

/// A problem's fill.
pub fn danger_fill() -> Hsla {
    Hsla::from(rgb(DANGER)).alpha(DANGER_FILL_ALPHA)
}

/// A problem's icon color.
pub fn danger_ink() -> Hsla {
    Hsla::from(rgb(DANGER)).blend(gpui::white().opacity(1.0 - DANGER_INK_MIX))
}
/// The overlay scrollbar thumb's white opacity, at rest and when hovered or dragged.
pub const SCROLL_THUMB: f32 = 0.32;
pub const SCROLL_THUMB_HOVER: f32 = 0.5;
/// `--explicit-fill`, `--explicit-ink`: the explicit "E" mark.
pub const EXPLICIT_FILL: u32 = 0xffffff73;
pub const EXPLICIT_INK: u32 = 0x000000d1;

/// Page alpha over the blurred desktop when translucency is on. Secondary text still has 4.5:1
/// contrast over a pure white wallpaper.
pub const BG_WINDOW_ALPHA: f32 = 0.82;

pub mod size {
    pub const TEXT_XS: f32 = 14.0;
    pub const TEXT_SM: f32 = 15.0;
    pub const TEXT_MD: f32 = 16.0;
    pub const TEXT_LG: f32 = 18.0;
    pub const TEXT_SECTION: f32 = 25.0;
    pub const TEXT_FEATURE: f32 = 21.0;
    pub const TEXT_SUBTITLE: f32 = 23.0;
    pub const TEXT_TITLE: f32 = 42.0;

    pub const R_XS: f32 = 6.0;
    pub const R_SM: f32 = 8.0;
    pub const R_MD: f32 = 12.0;
    pub const R_ROW: f32 = 13.0;
    pub const R_LG: f32 = 22.0;
    /// The corner radius of every floating panel.
    pub const R_FLOAT: f32 = 16.0;
    /// A submenu's width, its maximum height before it scrolls, and its gap from the parent menu.
    pub const SUBMENU_W: f32 = 260.0;
    pub const SUBMENU_MAX_H: f32 = 440.0;
    pub const SUBMENU_GAP: f32 = 4.0;
    /// The anchored popover's width and maximum height, for the smart rules panel and the Songs
    /// filters, which set their own width. Content scrolls past the maximum.
    pub const POP_W: f32 = 380.0;
    pub const POP_MAX_H: f32 = 440.0;
    /// Song filters. The chip's height, padding and glyph, the gap between chips and below the top
    /// bar, the filter popover's width and list height, the number field's width, the gap above the
    /// table header, and an active chip's maximum value width.
    pub const CHIP_H: f32 = 32.0;
    pub const CHIP_PAD: f32 = 13.0;
    pub const CHIP_GLYPH: f32 = 12.0;
    pub const CHIP_GAP: f32 = 6.0;
    pub const FILTER_ROW_GAP: f32 = 16.0;
    pub const CHIP_VALUE_MAX_W: f32 = 180.0;
    pub const FILTER_POP_W: f32 = 300.0;
    pub const FILTER_LIST_MAX_H: f32 = 300.0;
    pub const NUMBER_FIELD_W: f32 = 76.0;
    /// The smart playlist editor. Rule field and condition pills, the gap between rule rows, the
    /// indent per group level, the sort rows' pill and label column, and the limit field.
    pub const SMART_FIELD_W: f32 = 220.0;
    pub const SMART_OP_W: f32 = 175.0;
    pub const SMART_ROW_GAP: f32 = 8.0;
    pub const SMART_GROUP_INDENT: f32 = 14.0;
    pub const SMART_SORT_W: f32 = 220.0;
    pub const SMART_SORT_LABEL_W: f32 = 64.0;
    pub const SMART_LIMIT_W: f32 = 140.0;
    /// How far a toast floats above the bottom of the content column.
    pub const TOAST_GAP: f32 = 32.0;
    /// Listening Now's rows. The cover beside each person, and the live mark's three accent bars.
    pub const PERSON_ART: f32 = 30.0;
    pub const LIVE_MARK_H: f32 = 10.0;
    pub const LIVE_BAR_W: f32 = 2.0;
    pub const LIVE_BAR_GAP: f32 = 1.5;
    /// A Spotify friend's row. The friend mark on the avatar's corner, its glyph, and the initials
    /// shown without an avatar.
    pub const FRIEND_MARK: f32 = 13.0;
    pub const FRIEND_MARK_GLYPH: f32 = 8.0;
    pub const FRIEND_INITIALS: f32 = 11.0;
    /// A toast's height and side padding. One with an action keeps the height, with its small
    /// button inset evenly at the right end.
    pub const TOAST_H: f32 = 42.0;
    pub const TOAST_PAD: f32 = 18.0;
    /// How long a toast stays. One with an action stays longer, and hovering keeps it.
    pub const TOAST_MS: u64 = 3000;
    pub const TOAST_ACTION_MS: u64 = 5000;
    /// The space above each of the queue's section headings.
    pub const QUEUE_SECTION_GAP: f32 = 14.0;
    /// Placeholder rows under the Autoplay heading while its songs are being found.
    pub const QUEUE_FINDING_ROWS: usize = 3;
    /// A placeholder row's two text lines as quiet blocks. Their height, the gap between them and
    /// their widths as fractions of the row.
    pub const ROW_PLACEHOLDER_LINE: f32 = 9.0;
    pub const ROW_PLACEHOLDER_GAP: f32 = 8.0;
    pub const ROW_PLACEHOLDER_TITLE_W: f32 = 0.55;
    pub const ROW_PLACEHOLDER_ARTIST_W: f32 = 0.35;
    /// The glyph size and gap of the queue headings' icon buttons, Clear and Shuffle.
    pub const QUEUE_HEAD_ICON: f32 = 18.0;
    pub const QUEUE_HEAD_GAP: f32 = 8.0;
    /// The square X button that removes a queue row.
    pub const QUEUE_REMOVE_W: f32 = 28.0;
    /// Songs dragged out of their list. The card that follows the pointer at a
    /// `DRAG_OUT_OFFSET_X/Y` offset, the dimming of rows left behind, the drop target's ring, and how
    /// far a card's drop highlight spreads past its edges.
    pub const DRAG_OUT_CARD_W: f32 = 280.0;
    pub const DRAG_OUT_OFFSET_X: f32 = 14.0;
    pub const DRAG_OUT_OFFSET_Y: f32 = 12.0;
    pub const DRAG_OUT_DIM: f32 = 0.4;
    pub const DROP_RING: f32 = 2.0;
    pub const DROP_PAD_CARD: f32 = 8.0;
    /// `--dialog-*`, `--field-pad`, `--cover-pick-*`.
    pub const COVER_PICK_SIZE: f32 = 200.0;
    pub const COVER_PICK_GAP: f32 = 14.0;
    pub const COVER_PICK_ARROW_INSET: f32 = 10.0;
    pub const COVER_PICK_RING_W: f32 = 3.0;
    pub const COVER_PICK_RING_GAP: f32 = 3.0;
    pub const COVER_PICK_UPLOAD_ICON: f32 = 28.0;
    pub const COVER_PICK_UPLOAD_GAP: f32 = 8.0;
    pub const COVER_PICK_DOT: f32 = 7.0;
    pub const COVER_PICK_DOT_GAP: f32 = 10.0;
    pub const COVER_PICK_PAGER_TOP: f32 = 10.0;
    pub const COVER_PICK_PAGER_ICON: f32 = 13.0;
    pub const DIALOG_W: f32 = 420.0;
    pub const DIALOG_R: f32 = R_LG;
    pub const DIALOG_PAD: f32 = 22.0;
    pub const DIALOG_GAP: f32 = 14.0;
    pub const DIALOG_ACTIONS_GAP: f32 = 10.0;
    pub const DIALOG_ACTIONS_TOP: f32 = 4.0;
    /// `--menu-*`: every menu, select list and submenu. one text size (`--text-sm`, 13 px at the base scale, as the
    /// platforms' own menus); `--menu-detail-text` for a row's second line, counts and the account's host.
    pub const MENU_TEXT: f32 = TEXT_SM;
    pub const MENU_DETAIL_TEXT: f32 = TEXT_XS;
    pub const MENU_MIN_W: f32 = 236.0;
    pub const MENU_MAX_W: f32 = 380.0;
    pub const MENU_HEADER_W: f32 = 285.0;
    pub const MENU_PAD: f32 = 6.0;
    pub const MENU_ROW_H: f32 = 28.0;
    pub const MENU_ROW_PAD_X: f32 = 16.0;
    pub const MENU_ROW_PAD_Y: f32 = 6.0;
    pub const MENU_ROW_R: f32 = 10.0;
    pub const MENU_GAP: f32 = 12.0;
    pub const MENU_ICON: f32 = 16.0;
    pub const MENU_END_GAP: f32 = 18.0;
    pub const MENU_CARET: f32 = 13.0;
    pub const MENU_CHECK: f32 = 7.0;
    pub const MENU_SPINNER: f32 = 15.0;
    pub const MENU_RULE_INSET: f32 = 13.0;
    pub const MENU_RULE_GAP: f32 = 6.0;
    pub const MENU_EMPTY_PAD_Y: f32 = 10.0;
    pub const MENU_AVATAR: f32 = 34.0;
    pub const MENU_HEADER_PAD_Y: f32 = 8.0;
    /// `--menu-find-*`: the filter field at the top of a floating list.
    pub const MENU_FIND_H: f32 = 34.0;
    pub const MENU_FIND_GAP: f32 = 4.0;
    pub const MENU_FIND_PAD: f32 = 11.0;
    pub const MENU_FIND_GLYPH_GAP: f32 = 8.0;
    pub const MENU_FIND_ICON: f32 = 15.0;
    /// `--menu-heading-h`, `--menu-heading-icon`, `--menu-heading-gap`.
    pub const MENU_HEADING_H: f32 = 30.0;
    pub const MENU_HEADING_ICON: f32 = 15.0;
    pub const MENU_HEADING_GAP: f32 = 7.0;
    /// `--menu-quick-size`: a song menu's square quick action buttons.
    pub const MENU_QUICK_SIZE: f32 = 42.0;
    pub const MENU_QUICK_ICON: f32 = 20.0;
    /// `--pin-box-w`, `--pin-box-h`, `--pin-gap`, `--pin-text`.
    pub const PIN_BOX_W: f32 = 54.0;
    pub const PIN_BOX_H: f32 = 62.0;
    pub const PIN_GAP: f32 = 10.0;
    pub const PIN_TEXT: f32 = TEXT_SECTION;
    /// The padding around a command to copy, which uses `assets::font_mono`.
    pub const CODE_PAD: f32 = 12.0;
    /// The AirPlay output badge. The gap from glyph to name, the name's maximum width beside the
    /// bar's title, and the bar width below which only the glyph shows.
    pub const OUTPUT_BADGE_GAP: f32 = 6.0;
    pub const OUTPUT_BADGE_MAX_W: f32 = 200.0;
    pub const OUTPUT_BADGE_NARROW: f32 = 960.0;
    /// `--quality-glyph`, `--quality-glyph-twin-gap`: the quality badge's lossless wave, as parts of
    /// the badge text; hi-res stacks the same wave twice, centre to centre this far apart; under the
    /// bar width `QUALITY_BADGE_NARROW` the badge keeps the wave and drops its words.
    pub const QUALITY_GLYPH: f32 = 0.905;
    pub const QUALITY_GLYPH_TWIN_GAP: f32 = 0.419;
    pub const QUALITY_BADGE_NARROW: f32 = 840.0;
    /// `--search-pill-pad`, `--search-pill-gap`: a toolbar search field's side padding and the gap after its glyph.
    pub const SEARCH_PILL_PAD: f32 = 15.0;
    pub const SEARCH_PILL_GAP: f32 = 9.0;
    /// between a top bar title and its detail (a count, an owner).
    pub const TITLE_DETAIL_GAP: f32 = 10.0;
    /// a toolbar search field's narrowest: wide, under 1000 and under 620 of page.
    pub const SEARCH_PILL_W: f32 = 187.0;
    pub const SEARCH_PILL_W_MID: f32 = 168.0;
    pub const SEARCH_PILL_W_NARROW: f32 = 120.0;
    /// the Search page's field: wide, under 1000 of page, and as a share of the window at most.
    pub const SEARCH_FIELD_W: f32 = 384.0;
    pub const SEARCH_FIELD_W_MID: f32 = 260.0;
    pub const SEARCH_FIELD_MAX_SHARE: f32 = 0.42;
    /// the page width under which the Search page's field gives way to its tools, and how narrow it goes.
    pub const SEARCH_FIELD_CROWDED: f32 = 1040.0;
    pub const SEARCH_FIELD_MIN_W: f32 = 120.0;
    /// `--quality-pop-*`: the quality popover's widest, padding, column and row gaps, its gap above
    /// the badge and its distance from the window's edge.
    pub const QUALITY_POP_MAX_W: f32 = 400.0;
    pub const QUALITY_POP_PAD_TOP: f32 = 10.0;
    pub const QUALITY_POP_PAD_X: f32 = 14.0;
    pub const QUALITY_POP_PAD_BOTTOM: f32 = 11.0;
    pub const QUALITY_POP_COL_GAP: f32 = 14.0;
    pub const QUALITY_POP_ROW_GAP: f32 = 7.0;
    pub const QUALITY_POP_GAP: f32 = 8.0;
    pub const QUALITY_POP_EDGE: f32 = 10.0;
    /// the busy glyph's turn, in milliseconds.
    pub const SPIN_PERIOD: u64 = 900;
    /// `--button-small-h`, `--button-small-pad`.
    pub const BUTTON_SMALL_H: f32 = 30.0;
    pub const BUTTON_SMALL_PAD: f32 = 12.0;
    /// The gap between a button's glyph and its label.
    pub const BUTTON_GAP: f32 = 8.0;
    /// a full-size text button's side padding.
    pub const BUTTON_PAD: f32 = 18.0;
    pub const FIELD_PAD: f32 = 15.0;
    /// A labeled text field. Label to field, field to message, and an inner action's inset.
    pub const FIELD_LABEL_GAP: f32 = 7.0;
    pub const FIELD_MESSAGE_GAP: f32 = 7.0;
    pub const FIELD_INSET: f32 = 3.0;
    /// A status box's padding and the gap after its icon.
    pub const STATUS_PAD_Y: f32 = 11.0;
    pub const STATUS_PAD_X: f32 = 14.0;
    pub const STATUS_GAP: f32 = 12.0;
    /// A prompt card's padding, and the gap between its title, fact and answers.
    pub const PROMPT_CARD_PAD: f32 = 16.0;
    pub const PROMPT_CARD_GAP: f32 = 10.0;
    /// The lockup's wordmark size and gap from the mark, as fractions of the mark's size. Art is
    /// `assets/icons/nv-brand-*.svg`, from tools/brand_icons.py.
    pub const BRAND_WORD_RATIO: f32 = 1.1;
    pub const BRAND_GAP_RATIO: f32 = 0.3;
    /// The sign-in screen. The form's width, the gaps between fields and under the heading, the
    /// card's padding and the lockup's mark size.
    pub const CONNECT_W: f32 = 420.0;
    pub const CONNECT_GAP: f32 = 16.0;
    pub const CONNECT_HEADING_GAP: f32 = 8.0;
    pub const CONNECT_CARD_PAD: f32 = 28.0;
    pub const CONNECT_MARK: f32 = 55.0;
    /// The inset of window buttons in a corner of a screen with no top bar, like sign-in.
    pub const WINDOW_CORNER_GAP: f32 = 12.0;
    /// A disabled text button's opacity, or the alpha of a disabled primary or danger fill.
    pub const DISABLED_OPACITY: f32 = 0.4;
    /// The sheet dialog's width and maximum height, and its info rows' art, label column, padding
    /// and section gap.
    pub const SHEET_W: f32 = 560.0;
    pub const SHEET_MAX_H: f32 = 780.0;
    /// `--settings-max-w`.
    pub const SETTINGS_MAX_W: f32 = 900.0;
    pub const INFO_ART: f32 = 88.0;
    pub const INFO_LABEL_W: f32 = 148.0;
    pub const INFO_ROW_PAD_Y: f32 = 9.0;
    pub const INFO_ROW_PAD_X: f32 = 14.0;
    pub const INFO_SECTION_GAP: f32 = 18.0;
    /// The cover sheet. The picture's height, how far text rises over its fade, its focus point and
    /// fade start as fractions of its height, and the gaps between and within blocks.
    pub const SHEET_COVER_H: f32 = 420.0;
    pub const SHEET_COVER_OVERLAP: f32 = 120.0;
    pub const SHEET_COVER_FOCUS_Y: f32 = 0.22;
    pub const SHEET_COVER_FADE_FROM: f32 = 0.45;
    pub const SHEET_SECTION_GAP: f32 = 22.0;
    pub const SHEET_LABEL_GAP: f32 = 8.0;
    /// a hovered link's underline, in device pixels.
    pub const LINK_UNDERLINE: f32 = 1.0;
    /// `--download-mark`, `--download-glyph`, `--download-mark-gap`.
    pub const DOWNLOAD_MARK: f32 = 14.0;
    pub const DOWNLOAD_GLYPH: f32 = 20.0;
    pub const DOWNLOAD_MARK_GAP: f32 = 6.0;

    /// The page's side margin, the same at every window width.
    pub const GUTTER: f32 = 45.0;
    /// The split view for Albums and Artists. The list's width and gap before the divider, the row's
    /// height, padding, picture, text gap, highlight inset and separator start, and the All row's
    /// glyph. The page beside it uses `SPLIT_GUTTER` and `SPLIT_END`. Below `SPLIT_MIN_W` it's a grid.
    pub const SPLIT_LIST_W: f32 = 315.0;
    pub const SPLIT_LIST_PAD: f32 = 20.0;
    pub const SPLIT_ROW_H: f32 = 78.0;
    pub const SPLIT_ROW_PAD: f32 = 5.0;
    pub const SPLIT_ART: f32 = 45.0;
    pub const SPLIT_ROW_GAP: f32 = 13.0;
    pub const SPLIT_ROW_INSET: f32 = 6.0;
    pub const SPLIT_LINE_FROM: f32 = 60.0;
    pub const SPLIT_GLYPH: f32 = 26.0;
    pub const SPLIT_GUTTER: f32 = 20.0;
    pub const SPLIT_END: f32 = 25.0;
    pub const SPLIT_MIN_W: f32 = 760.0;
    pub const CONTENT_TOP: f32 = 32.0;
    pub const SIDEBAR_W: f32 = 245.0;
    /// The range the sidebar can be dragged to, and the grab strip on its right edge, the same as
    /// `DOCK_GRAB_W`.
    pub const SIDEBAR_W_MIN: f32 = 200.0;
    pub const SIDEBAR_W_MAX: f32 = 360.0;
    pub const SIDEBAR_GRAB_W: f32 = 12.0;
    /// `--marquee-fade`, `--marquee-gap`.
    pub const MARQUEE_FADE: f32 = 18.0;
    pub const MARQUEE_GAP: f32 = 48.0;
    /// `--sidebar-cover`, `--sidebar-cover-r`.
    pub const SIDEBAR_COVER: f32 = 22.0;
    pub const SIDEBAR_COVER_R: f32 = 5.0;
    /// `--sidebar-cover-glyph`: the play or pause glyph over a sidebar playlist cover.
    pub const SIDEBAR_COVER_GLYPH: f32 = 12.0;
    /// `--search-clear`, `--search-clear-lg`, `--search-clear-sm`: the clear button at the end of a search field with text (toolbar fields, the Search page's field, find fields in menus and filters).
    pub const SEARCH_CLEAR: f32 = 16.0;
    /// `--settings-section-gap`: between the Settings sections.
    pub const SETTINGS_SECTION_GAP: f32 = 30.0;
    /// `--settings-text-min`: the least a Settings row's text keeps beside its control; narrower, the control goes under the text.
    pub const SETTINGS_TEXT_MIN: f32 = 240.0;
    pub const SEARCH_CLEAR_LG: f32 = 20.0;
    pub const SEARCH_CLEAR_SM: f32 = 14.0;
    /// `--sidebar-heading-top`, `--sidebar-heading-pad-y`: the room above a sidebar heading, and the padding above and below its text (where a foldable one's hover fill reaches).
    pub const SIDEBAR_HEADING_TOP: f32 = 18.0;
    pub const SIDEBAR_HEADING_PAD_Y: f32 = 7.0;
    /// the leading padding of sidebar rows and headings.
    pub const SIDEBAR_ITEM_PAD_X: f32 = 14.0;
    /// an accent preset's round swatch in Settings.
    pub const ACCENT_SWATCH: f32 = 26.0;
    /// `--sidebar-active-weight`.
    pub const SIDEBAR_ACTIVE_WEIGHT: gpui::FontWeight = gpui::FontWeight::SEMIBOLD;
    pub const DOCK_W: f32 = 437.0;
    pub const DOCK_MIN_W: f32 = 340.0;
    pub const DOCK_MAX_W: f32 = 640.0;
    /// the grab strip on the dock's left edge, `--sidebar-grab-w`'s twin.
    pub const DOCK_GRAB_W: f32 = SIDEBAR_GRAB_W;
    /// the sidebar's backdrop blur and saturation.
    pub const SIDEBAR_BLUR: f32 = 38.0;
    pub const SIDEBAR_SATURATION: f32 = 1.6;
    pub const GLASS_BLUR: f32 = 24.0;
    pub const GLASS_SATURATION: f32 = 1.5;
    /// The backdrop blur of menus, popovers, dialogs and toasts.
    pub const FLOAT_BLUR: f32 = 30.0;
    pub const FLOAT_SATURATION: f32 = 1.6;
    /// `--menu-blur`: the float blur with more colour kept, so the frost reads as what's beneath it.
    pub const MENU_BLUR: f32 = FLOAT_BLUR;
    pub const MENU_SATURATION: f32 = 1.8;
    /// `--bar-blur`.
    pub const BAR_BLUR: f32 = 44.0;
    pub const BAR_SATURATION: f32 = 1.6;
    /// The expanded player's text scrim. Wide, soft and light so it never reads as a box.
    pub const GLOW_SCRIM_REACH: f32 = 96.0;
    pub const GLOW_SCRIM_BLUR: f32 = 64.0;
    pub const GLOW_SCRIM_MAX: f32 = 0.32;
    /// A synced lyric line's opacity by distance from the current line. Near, minus a step per line,
    /// never below the minimum.
    pub const LYRIC_DIM_NEAR: f32 = 0.46;
    pub const LYRIC_DIM_STEP: f32 = 0.07;
    pub const LYRIC_DIM_MIN: f32 = 0.14;
    /// White's opacity on the unsung part of the current lyric line.
    pub const LYRIC_UNSUNG: f32 = 0.58;
    /// A hovered lyric line's opacity.
    pub const LYRIC_HOVER: f32 = 0.9;
    /// A lyric line's blur by distance from the current line, in CSS pixels.
    pub const LYRIC_BLUR_NEAR: f32 = 1.2;
    pub const LYRIC_BLUR_STEP: f32 = 0.9;
    pub const LYRIC_BLUR_MAX: f32 = 5.5;
    /// a lyric line's height as a share of its type size; `--lyric-glow` measures from it.
    pub const LYRIC_LINE_HEIGHT: f32 = 1.22;
    /// `--lyric-glow`: radii are shares of the line height.
    pub const LYRIC_GLOW_TIGHT: f32 = 0.025;
    pub const LYRIC_GLOW_TIGHT_ALPHA: f32 = 0.7;
    pub const LYRIC_GLOW_WIDE: f32 = 0.05;
    pub const LYRIC_GLOW_WIDE_ALPHA: f32 = 0.38;
    /// `--lyric-fill-pace` (characters a second), `--lyric-hold-min`, `--lyric-glow-fade` (seconds).
    pub const LYRIC_FILL_PACE: f32 = 14.0;
    pub const LYRIC_HOLD_MIN: f32 = 0.5;
    pub const LYRIC_GLOW_FADE: f32 = 0.3;
    /// `--lyric-peek`, `--lyric-top-clear`: shares of the type size.
    pub const LYRIC_PEEK: f32 = 1.0;
    pub const LYRIC_TOP_CLEAR: f32 = 0.1;
    /// `--lyric-backing-scale`.
    pub const LYRIC_BACKING_SCALE: f32 = 0.62;
    /// `--lyric-gap-*`: sizes are shares of the type size, times in seconds.
    pub const LYRIC_GAP_MIN: f32 = 5.0;
    pub const LYRIC_GAP_BARS: usize = 4;
    pub const LYRIC_GAP_WIDTH: f32 = 0.23;
    pub const LYRIC_GAP_BAR_GAP: f32 = 0.18;
    pub const LYRIC_GAP_BAR_MIN: f32 = 0.23;
    pub const LYRIC_GAP_HEIGHT: f32 = 1.14;
    pub const LYRIC_GAP_SWELL: f32 = 0.65;
    pub const LYRIC_GAP_POP: f32 = 0.14;
    /// `--lyric-gap-swell-grow`, `--lyric-gap-pop-lead` (seconds).
    pub const LYRIC_GAP_SWELL_GROW: f32 = 0.35;
    pub const LYRIC_GAP_POP_LEAD: f32 = 0.05;
    /// `--lyric-gap-bounce-*`: bar i loops at `rate + i * spread` radians a second, offset by `i * phase`.
    pub const LYRIC_GAP_BOUNCE_RATE: f32 = 2.1;
    pub const LYRIC_GAP_BOUNCE_SPREAD: f32 = 0.37;
    pub const LYRIC_GAP_BOUNCE_PHASE: f32 = 1.9;
    /// `--lyric-gap-bar-still`: the bars' level, 0 to 1, when they don't bounce.
    pub const LYRIC_GAP_BAR_STILL: f32 = 0.5;
    /// `--lyric-plain`, `--lyric-plain-gap`: unsynced lyrics' ink, and the full screen player's gap between their lines.
    pub const LYRIC_PLAIN: f32 = 0.85;
    pub const LYRIC_PLAIN_GAP: f32 = 18.0;
    /// `--bar-glow-strength`.
    pub const BAR_GLOW_ALPHA: f32 = 0.24;
    /// The mini player. Its compact and large sizes, corners, padding and gaps, the compact one's
    /// buttons and window controls, how solid its tinted glass is, the large one's scrim, and how far
    /// from a screen corner it first opens.
    pub const MINI_W: f32 = 366.0;
    pub const MINI_H: f32 = 100.0;
    pub const MINI_LARGE: f32 = 366.0; // the large one is a square.
    pub const MINI_R: f32 = R_FLOAT;
    pub const MINI_PAD: f32 = 12.0;
    pub const MINI_GAP: f32 = 12.0;
    pub const MINI_BUTTON: f32 = 34.0;
    pub const MINI_CHROME_BUTTON: f32 = 28.0;
    pub const MINI_GLASS: f32 = 0.86;
    pub const MINI_SCRIM_START: f32 = 0.34;
    pub const MINI_SCRIM_STRENGTH: f32 = 0.92;
    pub const MINI_CORNER_MARGIN: f32 = 16.0;
    /// The one-time hint's width, at least the card's, and its gap from the card, on desktops
    /// without layer-shell.
    pub const MINI_HINT_W: f32 = 380.0;
    /// Resizing, from the compact size up to `MINI_MAX` either way. At least `MINI_STACK_H` tall with
    /// height at least `MINI_STACK_RATIO` of width it uses the stacked layout. Otherwise it's compact
    /// with a square cover. `MINI_GRIP` is the resize band, doubled at the corners.
    pub const MINI_MAX: f32 = 640.0;
    pub const MINI_STACK_H: f32 = 220.0;
    pub const MINI_STACK_RATIO: f32 = 0.6;
    pub const MINI_GRIP: f32 = 8.0;
    pub const MINI_HINT_GAP: f32 = 10.0;
    /// The mini player's hairline seek bar, its hover thumb and its time chip's edge limit. A sync's
    /// progress bar height, and its width under the empty state's text.
    pub const SYNC_BAR_H: f32 = 3.0;
    pub const SYNC_METER_W: f32 = 280.0;
    pub const SCRUB_THIN: f32 = 3.0;
    pub const SCRUB_THIN_HOVER: f32 = 5.0;
    /// `--scrub-line`, `--scrub-line-hover`, `--scrub-slot` (the grab height, taller than the line).
    pub const SCRUB_LINE: f32 = 4.0;
    pub const SCRUB_LINE_HOVER: f32 = 7.0;
    pub const SCRUB_SLOT: f32 = 14.0;
    pub const SCRUB_CHIP_EDGE: f32 = 46.0;
    /// A pressed small control's opacity, play and skip's scale when hovered and pressed, and an
    /// icon button's opacity at rest.
    pub const PRESS_DIM: f32 = 0.7;
    pub const HOVER_GROW: f32 = 1.08;
    pub const PRESS_SHRINK: f32 = 0.94;
    /// `PressPhase` scales: `--press-hold`, `--press-squash`, `--press-over`.
    pub const PRESS_HOLD: f32 = 0.86;
    pub const PRESS_SQUASH: f32 = 0.72;
    pub const PRESS_OVER: f32 = 1.08;
    /// gaps in the dock's stack under its artwork.
    pub const PLAYER_META_GAP: f32 = 16.0;
    pub const PLAYER_TRANSPORT_GAP: f32 = 12.0;
    pub const PLAYER_VOLUME_GAP: f32 = 8.0;
    /// `--expanded-panel-top`, `--expanded-edge`.
    pub const EXPANDED_PANEL_TOP: f32 = 88.0;
    pub const EXPANDED_EDGE: f32 = 24.0;
    /// `--lyric-expanded`: a share of the width, clamped; plain lyrics are `LYRIC_PLAIN_SCALE` of it.
    pub const LYRIC_EXPANDED_SHARE: f32 = 0.027;
    pub const LYRIC_EXPANDED_MIN: f32 = 28.0;
    pub const LYRIC_EXPANDED_MAX: f32 = 44.0;
    pub const LYRIC_PLAIN_SCALE: f32 = 1.0;
    /// `--lyric-word-lift*`: a share of the type size; none while motion is reduced.
    pub const LYRIC_WORD_LIFT: f32 = 0.09;
    /// `--lyrics-credit-scale`, `--lyrics-credit-opacity`.
    pub const LYRICS_CREDIT_SCALE: f32 = 0.5;
    pub const LYRICS_CREDIT_OPACITY: f32 = 0.6;
    /// `--cover-paused-scale`.
    pub const COVER_PAUSED_SCALE: f32 = 0.73;
    pub const ICON_REST: f32 = 0.88;
    /// How much smaller the switch is inside a menu row.
    pub const SWITCH_COMPACT: f32 = 0.86;
    /// Shuffle and repeat's opacity while off, and the player switcher's unselected segments.
    pub const MODE_REST: f32 = 0.62;
    /// A disabled mode button, like Shuffle during a station.
    pub const MODE_DISABLED: f32 = 0.35;
    pub const SWITCHER_REST: f32 = 0.85;
    /// The player switcher's height, a 40-token segment inside 4 tokens of padding.
    pub const SWITCHER_H: f32 = 48.0;
    /// Blurs for soft copies of artwork. The dock's reflection, Discover's hero wash, the page glow
    /// and the expanded backdrop. The art bakes blur small copies, so their sigmas approximate these.
    pub const REFLECT_BLUR: f32 = 32.0;
    pub const REFLECT_SATURATION: f32 = 1.2;
    /// The dock reflection's opacity.
    pub const REFLECT_OPACITY: f32 = 0.52;
    /// The artist header. The photo's maximum aspect, its gap beside the name, the name's gap to its
    /// controls, and the gaps of the Top Songs and Latest Release columns side by side or stacked.
    pub const ARTIST_ART_MAX_RATIO: f32 = 1.6;
    pub const ARTIST_HEAD_GAP: f32 = 32.0;
    pub const ARTIST_HEAD_TITLE_GAP: f32 = 16.0;
    pub const ARTIST_COLUMNS_GAP: f32 = 44.0;
    pub const ARTIST_COLUMNS_STACK_GAP: f32 = 24.0;
    pub const PAGE_GLOW_SATURATION: f32 = 1.2;
    /// The strength of Discover's hero wash, the page glow and the expanded player's backdrop glow.
    pub const HERO_GLOW_OPACITY: f32 = 0.6;
    pub const PAGE_GLOW_OPACITY: f32 = 0.72;
    pub const PLAYER_GLOW_OPACITY: f32 = 0.55;
    /// The brightness multiplier applied in the page and player glows.
    pub const PAGE_GLOW_BRIGHTNESS: f32 = 0.72;
    pub const PLAYER_GLOW_BRIGHTNESS: f32 = 0.82;
    pub const PLAYER_GLOW_SATURATION: f32 = 1.3;
    /// The expanded backdrop glow's deviation, in CSS pixels.
    pub const PLAYER_GLOW_BLUR: f32 = 110.0;
    pub const EDGE_BLUR: f32 = 24.0;
    pub const EDGE_SATURATION: f32 = 1.4;
    /// The lower part of the top edge that fades out.
    pub const EDGE_FADE: f32 = 0.45;
    /// A slight page-color dimming so controls read over the blur.
    pub const EDGE_TINT: f32 = 0.62;
    /// The margin the sidebar, dock and bottom bar keep from the window's edges.
    pub const WINDOW_INSET: f32 = 10.0;
    /// The window's own corner radius, 10px at 100% on every desktop.
    pub const WINDOW_RADIUS: f32 = 10.0 / 0.875;
    /// The window's outline and shadow, in real pixels outside the interface scale. Only for Linux
    /// client-side decorations, since Windows and macOS draw their own.
    pub const FRAME_OUTLINE_WIDTH: f32 = 1.0;
    pub const FRAME_OUTLINE_ALPHA: f32 = 0.11;
    pub const FRAME_SHADOW_BLUR: f32 = 20.0;
    pub const FRAME_SHADOW_ALPHA: f32 = 0.42;
    pub const FRAME_SHADOW_ALPHA_INACTIVE: f32 = 0.2;
    /// Room around the window for the shadow. GPUI's blur reaches about 1.8 times its radius, so a
    /// smaller margin would cut it off. Only `FRAME_GRIP_OUTSIDE` of it takes the pointer.
    pub const FRAME_SHADOW_MARGIN: f32 = 36.0;
    /// How far the resize grips reach out over the shadow and into the window. Thin like native
    /// grips, which run about 4 to 8 px.
    pub const FRAME_GRIP_OUTSIDE: f32 = 8.0;
    pub const FRAME_GRIP_INSIDE: f32 = 4.0;
    /// How far a corner grip extends past the edge grips, so corners are easier to catch.
    pub const FRAME_GRIP_CORNER: f32 = 10.0;
    pub const BAR_W: f32 = 1010.0;
    pub const BAR_H: f32 = 62.0;
    /// The player chip over the bar, centered above the play controls. Its maximum width and gap.
    pub const CHIP_ABOVE_MAX_W: f32 = 360.0;
    pub const CHIP_ABOVE_GAP: f32 = 10.0;
    pub const TOPBAR_H: f32 = 76.0;
    pub const PILL_H: f32 = 56.0;
    pub const CONTROL_H: f32 = 40.0;
    /// `--segmented-*`: `GLASS` is the larger page size, a full pill.
    pub const SEGMENTED_H: f32 = 28.0;
    pub const SEGMENTED_R: f32 = R_XS;
    pub const SEGMENTED_GLASS_H: f32 = 32.0;
    pub const SEGMENTED_PAD_X: f32 = 15.0;
    pub const SEGMENTED_DIVIDER_W: f32 = 1.0;
    pub const SEGMENTED_DIVIDER_INSET: f32 = 6.0;
    pub const ICON_W: f32 = 38.0;
    pub const ICON_BUTTON_H: f32 = 34.0;
    /// `--bar-art`, `--bar-art-inset`, `--bar-line-gap`.
    pub const BAR_ART: f32 = 38.0;
    pub const BAR_ART_INSET: f32 = 11.0;
    pub const BAR_LINE_GAP: f32 = 5.0;
    /// `--bar-art-grow`, `--bar-art-expand`.
    pub const BAR_ART_EXPAND: f32 = 22.0;
    pub const BAR_ART_GROW: f32 = 1.08;
    /// `--bar-scrub-rest`, `--bar-scrub-hover`, `--bar-scrub-rise`, `--bar-scrub-reach`, `--bar-rows-dim`.
    pub const BAR_SCRUB_REST: f32 = 2.0;
    pub const BAR_SCRUB_HOVER: f32 = 5.0;
    pub const BAR_SCRUB_RISE: f32 = 6.0;
    pub const BAR_SCRUB_REACH: f32 = 8.0;
    pub const BAR_ROWS_DIM: f32 = 0.45;
    /// `--active-fill-size`: a multiple of the glyph.
    pub const ACTIVE_FILL_SIZE: f32 = 1.7;
    /// `--active-fill-start`: a share of the full size.
    pub const ACTIVE_FILL_START: f32 = 0.9;
    /// `--active-fill-pad`: twice this at the sides.
    pub const ACTIVE_FILL_PAD: f32 = 3.0;
    pub const ACTION_W: f32 = 41.0;
    pub const ROW_H: f32 = 78.0;
    /// song table number columns: Time and Plays, right-aligned with `NUM_CELL_PAD` after them; the song name ends with `NAME_CELL_PAD`, other cells with `CELL_PAD`.
    pub const TIME_COL_W: f32 = 78.0;
    pub const PLAYS_COL_W: f32 = 76.0;
    pub const NUM_CELL_PAD: f32 = 20.0;
    pub const CELL_PAD: f32 = 14.0;
    pub const NAME_CELL_PAD: f32 = 12.0;
    /// `--popularity-col-w`, `--popularity-bar-w`, `--popularity-bar-gap`, `--popularity-bar-h`: the Heat meter, drawn on whole device pixels.
    pub const POPULARITY_COL_W: f32 = 64.0;
    pub const POPULARITY_BAR_W: f32 = 2.0;
    pub const POPULARITY_BAR_GAP: f32 = 2.0;
    pub const POPULARITY_BAR_H: f32 = 14.0;
    /// `--list-page-w` and the list page's gaps.
    pub const LIST_PAGE_W: f32 = 980.0;
    pub const LIST_PAGE_GAP: f32 = 30.0;
    pub const LIST_ROW_PAD_X: f32 = 10.0;
    /// A list page's song or collection row height.
    pub const LIST_ROW_H: f32 = 68.0;
    /// Share links. The cover's gap to its title, the end-date and visits columns, and the button gap.
    pub const SHARE_SUBJECT_GAP: f32 = 12.0;
    pub const SHARE_ENDS_W: f32 = 170.0;
    pub const SHARE_VISITS_W: f32 = 170.0;
    pub const SHARE_ACTIONS_GAP: f32 = 2.0;
    pub const ROW_COMPACT_H: f32 = 44.0;
    pub const THUMB: f32 = 42.0;
    /// Card and grid sizes, and their narrow-window values (`NARROW_*`) with title sizes.
    pub const CARD_W: f32 = 200.0;
    pub const CARD_GAP: f32 = 32.0;
    pub const TILE_GAP: f32 = 20.0;
    pub const GRID_CARD: f32 = 224.0;
    pub const SECTION_GAP: f32 = 42.0;
    /// A section heading's gap above its rows or cards, and to a button at its right.
    pub const SHELF_HEAD_GAP: f32 = 16.0;
    /// `--shelf-fade` and `--shelf-gap` with the dock fully in: a shelf fades out over the first and stops the second short of the dock.
    pub const SHELF_FADE: f32 = 130.0;
    pub const SHELF_GAP: f32 = 4.0;
    /// the shelf fade's mask stops (place from the clear edge, opacity).
    pub const SHELF_FADE_STOPS: [(f32, f32); 2] = [(0.28, 0.22), (0.62, 0.62)];
    /// how far past the window the page list builds blocks, so a later section is asked for once.
    pub const NEAR_VIEW_MARGIN: f32 = 420.0;
    /// The gap below a playlist's action pills before the sorted-order note. The table follows the
    /// note directly, so the column labels sit about as far below it.
    pub const ORDER_NOTE_GAP: f32 = 28.0;
    pub const HERO_ART: f32 = 250.0;
    pub const NARROW_CARD_W: f32 = 148.0;
    pub const NARROW_CARD_GAP: f32 = 14.0;
    pub const NARROW_TILE_GAP: f32 = 14.0;
    pub const NARROW_GRID_CARD: f32 = 150.0;
    pub const NARROW_SECTION_GAP: f32 = 30.0;
    pub const NARROW_TEXT_SECTION: f32 = 20.0;
    pub const NARROW_TEXT_TITLE: f32 = 30.0;
    pub const NARROW_HERO_ART: f32 = 180.0;
    /// Compact two-line rows, such as album tracks and genre rows.
    pub const MINI_ROW_H: f32 = 65.0;
    /// Genre tiles' width over height, and the genre rows' minimum width.
    pub const GENRE_TILE_RATIO: f32 = 4.0 / 3.0;
    pub const GENRE_CHIP_W: f32 = 250.0;
    /// The name's inset from a genre tile's top and left, as fractions of the tile's width.
    pub const GENRE_TILE_PAD_Y: f32 = 0.07;
    pub const GENRE_TILE_PAD_X: f32 = 0.08;
    pub const NARROW_GENRE_CHIP_W: f32 = 200.0;
    /// A genre row's side padding and the gap between its cover and text.
    pub const GENRE_ROW_PAD: f32 = 8.0;
    pub const GENRE_ROW_GAP: f32 = 13.0;
    /// The fan of covers in a genre tile's corner. A cover's width as a fraction of the tile's, the
    /// front cover's overhang past the right and bottom edges, each card's shift and rotation front to
    /// back at rest and on hover, and the single-cover case.
    pub const GENRE_FAN_SIZE: f32 = 0.46;
    pub const GENRE_FAN_RIGHT: f32 = -0.05;
    pub const GENRE_FAN_BOTTOM: f32 = -0.17;
    pub const GENRE_FAN: [FanCard; 3] = [FanCard { x: 0.0, y: 0.0, deg: 10.0 }, FanCard { x: -0.30, y: 0.0, deg: -2.0 }, FanCard { x: -0.56, y: 0.0, deg: -14.0 }];
    pub const GENRE_FAN_HOVER: [FanCard; 3] = [FanCard { x: 0.0, y: -0.03, deg: 13.0 }, FanCard { x: -0.34, y: -0.02, deg: -3.0 }, FanCard { x: -0.64, y: 0.0, deg: -18.0 }];
    pub const GENRE_FAN_SINGLE: FanCard = FanCard { x: 0.0, y: 0.0, deg: 6.0 };
    /// A tinted tile's gradient angle and how much the far end darkens.
    pub const TILE_FILL_ANGLE: f32 = 150.0;
    pub const TILE_FILL_SHADE: f32 = 0.7;
    /// The maximum lightness a tint background may have under light text.
    pub const TINT_TEXT_SAFE_L: f32 = 0.47;
    /// `--album-*`.
    pub const ALBUM_HEAD_GAP: f32 = 25.0;
    pub const ALBUM_INFO_PAD_TOP: f32 = 20.0;
    pub const ALBUM_ARTIST_GAP: f32 = 14.0;
    pub const ALBUM_META_GAP: f32 = 12.0;
    pub const ALBUM_SECTION_GAP: f32 = 32.0;
    /// `--countdown-*`; `PAD_X` once it says "Out now".
    pub const COUNTDOWN_R: f32 = R_FLOAT;
    pub const COUNTDOWN_GAP: f32 = 20.0;
    pub const COUNTDOWN_PAD_Y: f32 = 14.0;
    pub const COUNTDOWN_PAD_X: f32 = 28.0;
    pub const COUNTDOWN_CELL_W: f32 = 92.0;
    pub const COUNTDOWN_NUMBER: f32 = 34.0;
    pub const COUNTDOWN_LABEL: f32 = TEXT_XS;
    pub const COUNTDOWN_LABEL_GAP: f32 = 4.0;
    /// `--row-unavailable-opacity`.
    pub const ROW_UNAVAILABLE_OPACITY: f32 = 0.4;
    /// `--cover-label-inset`, `--cover-label-pad-y`, `--cover-label-pad-x`.
    pub const COVER_LABEL_INSET: f32 = 8.0;
    pub const COVER_LABEL_PAD_Y: f32 = 2.0;
    pub const COVER_LABEL_PAD_X: f32 = 8.0;
    /// `--cover-mark-*`: shares of the cover's width. GPUI text has no letter spacing, so `--cover-mark-tracking` has no counterpart.
    pub const COVER_MARK_INSET: f32 = 0.057;
    pub const COVER_MARK_TEXT: f32 = 0.052;
    pub const COVER_MARK_GLYPH: f32 = 0.076;
    pub const COVER_MARK_GAP: f32 = 0.029;
    /// `--art-glyph`: the stand-in glyph's icon box as a share of its artwork's size.
    pub const ART_GLYPH: f32 = 0.406;
    /// `--card-text-gap`.
    pub const CARD_TEXT_GAP: f32 = 10.0;
    /// `--station-*`: shares of the tile's width, except `STATION_FACE_HOVER` (a scale), `STATION_SHADE` (the tint's
    /// share at the bottom of the shade) and the `*_SHADE_END` stops (shares of the height).
    pub const STATION_PAD: f32 = 0.057;
    pub const STATION_NAME: f32 = 0.10;
    pub const STATION_NAME_BOTTOM: f32 = 0.052;
    pub const STATION_FACE: f32 = 0.133;
    pub const STATION_FACE_GAP: f32 = 0.019;
    pub const STATION_FACES_BOTTOM: f32 = 0.19;
    pub const STATION_RING: f32 = 0.007;
    pub const STATION_INSET: f32 = 0.029;
    pub const STATION_FACE_HOVER: f32 = 1.12;
    pub const STATION_FACE_SPREAD: f32 = 0.014;
    pub const STATION_SHADE: f32 = 0.88;
    pub const STATION_SHADE_END: f32 = 0.55;
    pub const STATION_MARK_SHADE_END: f32 = 0.34;
    /// `--shadow-art` as (color, y offset, blur, spread).
    pub const SHADOW_ART: (u32, f32, f32, f32) = (0x000000b3, 20.0, 48.0, -20.0);
    /// The floating panel shadow as (color, y offset, blur, spread).
    pub const SHADOW_FLOAT: (u32, f32, f32, f32) = (0x000000b3, 18.0, 50.0, -14.0);

    /// One card of a fan.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct FanCard {
        pub x: f32,
        pub y: f32,
        pub deg: f32,
    }

    pub const DOCK_MIN_WINDOW: f32 = 1180.0;
    pub const SIDEBAR_MIN_WINDOW: f32 = 860.0;
    /// The narrow-window width, where the sidebar becomes a drawer.
    pub const NARROW_WINDOW: f32 = SIDEBAR_MIN_WINDOW;
    /// The narrowest page beside the dock, at the narrowest window that can dock with the sidebar
    /// open and the dock at its default width.
    pub const DOCK_MIN_CONTENT: f32 = DOCK_MIN_WINDOW - SIDEBAR_W - WINDOW_INSET * 2.0 - DOCK_W;
}

/// Animation lengths in seconds and the Reduce Motion switch. `progress` and `follow` finish at
/// once while motion is reduced, and GPUI's `with_animation` follows `App::reduce_motion`. Things
/// that show information over time, like progress bars, don't go through here.
pub mod motion {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    /// Hover timing. Changes take `HOVER_IN` going on and `HOVER_OUT` going off, and a press
    /// `HOVER_PRESS`, all on the `HOVER_EASE` cubic-bezier curve.
    pub const HOVER_IN: f32 = 0.10;
    pub const HOVER_OUT: f32 = 0.24;
    pub const HOVER_PRESS: f32 = 0.10;
    pub const HOVER_EASE: [f32; 4] = [0.2, 0.7, 0.2, 1.0];
    /// `--ease`.
    pub const EASE: [f32; 4] = [0.2, 0.7, 0.2, 1.0];
    /// `--fast`, `--med`.
    pub const FAST: f32 = 0.18;
    pub const MED: f32 = 0.34;
    /// `--popularity-heat-sweep`, `--popularity-heat-sweep-step`: a light passes bar to bar once a period, rising over the first `HEAT_FLASH_PEAK` of it and gone by `HEAT_FLASH_END`.
    pub const POPULARITY_HEAT_SWEEP: f32 = 4.0;
    pub const POPULARITY_HEAT_SWEEP_STEP: f32 = 0.09;
    pub const HEAT_FLASH_PEAK: f32 = 0.05;
    pub const HEAT_FLASH_END: f32 = 0.12;
    /// `--toggle-fade`.
    pub const TOGGLE_FADE: f32 = 0.2;
    /// `--lyric-move`, `--lyric-ease`, `--lyric-stagger`.
    pub const LYRIC_MOVE: f32 = 0.62;
    /// `--lyric-move-lead`: so the line is in place before it's sung.
    pub const LYRIC_MOVE_LEAD: f32 = LYRIC_MOVE;
    pub const LYRIC_EASE: [f32; 4] = [0.3, 1.18, 0.4, 1.0];
    /// `--lyric-bounce`.
    pub const LYRIC_BOUNCE: f32 = 0.35;
    pub const LYRIC_WORD_LIFT_TIME: f32 = 1.5;
    pub const LYRIC_WORD_LIFT_LEAD: f32 = 0.3;
    pub const LYRIC_WORD_LIFT_EASE: [f32; 4] = [0.25, 0.1, 0.25, 1.0];
    pub const LYRIC_WORD_FALL: f32 = 0.7;
    /// `--lyric-lift-bounce` (a share of the lift), `--lyric-lift-settle`.
    pub const LYRIC_LIFT_BOUNCE: f32 = 0.18;
    pub const LYRIC_LIFT_SETTLE: f32 = 0.32;
    /// `--lyric-wave-*`: the held last word's rolling letters; height a share of the type size, phase a share of a cycle per letter.
    pub const LYRIC_WAVE_HEIGHT: f32 = 0.024;
    pub const LYRIC_WAVE_PERIOD: f32 = 0.643;
    pub const LYRIC_WAVE_PHASE: f32 = 0.16;
    pub const LYRIC_WAVE_IN: f32 = 0.25;
    pub const LYRIC_WAVE_OUT: f32 = 0.45;
    pub const EASE_IN_OUT: [f32; 4] = [0.42, 0.0, 0.58, 1.0];
    pub const EASE_OUT: [f32; 4] = [0.0, 0.0, 0.58, 1.0];
    /// an even speed, for a turning glyph.
    pub const LINEAR: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
    pub const LYRIC_STAGGER: f32 = 0.03;
    /// `--cover-pause`.
    pub const COVER_PAUSE: f32 = 0.5;
    /// seconds per `PressPhase` step: `--press-in` going down, `--press-out` going up; `--press-swap`.
    pub const PRESS_IN: f32 = 0.09;
    pub const PRESS_OUT: f32 = 0.12;
    pub const PRESS_SWAP: f32 = 0.1;
    /// `--active-fill-grow`, `--active-fill-fade`.
    pub const ACTIVE_FILL_GROW: f32 = 0.25;
    pub const ACTIVE_FILL_FADE: f32 = 0.25;
    /// `--player-enter`.
    pub const PLAYER_ENTER: f32 = 0.38;
    /// `--marquee-speed` (tokens a second), `--marquee-pause` (seconds).
    pub const MARQUEE_SPEED: f32 = 30.0;
    pub const MARQUEE_PAUSE: f32 = 3.0;
    /// `--bar-scrub-delay`.
    pub const BAR_SCRUB_DELAY: f32 = 0.35;
    /// `--reveal-grace`.
    pub const REVEAL_GRACE: f32 = 0.3;
    /// Rows sliding apart and closing up during a drag.
    pub const DRAG_SLIDE: f32 = 0.17;
    /// A page's tint fading to another.
    pub const TINT_FADE: f32 = 0.32;
    /// Art fading in over its placeholder.
    pub const ART_FADE: f32 = 0.18;
    /// `--panel-slide`: the sidebar, side player and bar sliding in and out while the page resizes with them.
    pub const PANEL_SLIDE: f32 = 0.3;
    /// `--song-fade`: the player's artwork and title cross-fading on a song change.
    pub const SONG_FADE: f32 = 0.25;
    /// `--view-fade`, `--view-rise` (tokens): the player's views cross-fading, the incoming one rising.
    pub const VIEW_FADE: f32 = 0.22;
    pub const VIEW_RISE: f32 = 8.0;
    /// `--page-fade`, `--page-rise` (tokens): a new page fading in rising, or falling after Back.
    pub const PAGE_FADE: f32 = 0.15;
    pub const PAGE_RISE: f32 = 6.0;
    /// `--land-fade`, `--land-stagger`: a page's sections fading in as their content arrives, one after another.
    pub const LAND_FADE: f32 = 0.24;
    pub const LAND_STAGGER: f32 = 0.04;
    /// sections after this many share the last one's delay.
    pub const LAND_STAGGER_MAX: usize = 4;
    /// `--like-pop`, `--like-pop-scale`: the heart growing and back as a song is liked, peaking at `LIKE_POP_PEAK` of the time.
    pub const LIKE_POP: f32 = 0.3;
    pub const LIKE_POP_SCALE: f32 = 1.28;
    pub const LIKE_POP_PEAK: f32 = 0.4;
    /// `--row-flash`: rows lit as songs are queued or added to a playlist, fading back.
    pub const ROW_FLASH: f32 = 0.35;
    /// An overlay scrollbar fading out once scrolling stops.
    pub const SCROLLBAR_FADE: f32 = 0.2;
    /// a time constant.
    pub const LYRIC_DIM: f32 = 0.11;
    /// Sync progress. Without a total, a segment `SYNC_SWEEP_SPAN` of the bar wide sweeps across every
    /// `SYNC_SWEEP` seconds. A later sync shows only once it has run `SYNC_SHOW_AFTER`.
    pub const SYNC_SWEEP: f32 = 1.4;
    pub const SYNC_SWEEP_SPAN: f32 = 0.3;
    pub const SYNC_SHOW_AFTER: f32 = 10.0;

    static REDUCED: AtomicBool = AtomicBool::new(false);

    pub fn reduced() -> bool {
        REDUCED.load(Ordering::Relaxed)
    }

    /// Sets reduced motion from the settings portal and redraws the windows when it changes.
    pub fn set_reduced(cx: &mut gpui::App, on: bool) {
        if REDUCED.swap(on, Ordering::Relaxed) != on {
            eprintln!("navia: reduce motion {}", if on { "on" } else { "off" });
        }
        cx.set_reduce_motion(on);
    }

    /// the lift left `elapsed` seconds after a line stopped being current, as a share of the full lift; None once at rest.
    pub fn lyric_fall(elapsed: f32) -> Option<f32> {
        if reduced() || elapsed >= LYRIC_WORD_FALL + LYRIC_LIFT_SETTLE {
            return None;
        }
        Some(if elapsed < LYRIC_WORD_FALL {
            1.0 - (1.0 + LYRIC_LIFT_BOUNCE) * cubic_bezier(EASE_OUT, elapsed / LYRIC_WORD_FALL)
        } else {
            -LYRIC_LIFT_BOUNCE * (1.0 - cubic_bezier(EASE_IN_OUT, (elapsed - LYRIC_WORD_FALL) / LYRIC_LIFT_SETTLE))
        })
    }

    /// how far letter `letter` of a held last word is raised, as a share of the type size, `t` seconds after the word
    /// started in a hold of `held` seconds; at rest at both ends.
    pub fn lyric_wave(letter: usize, t: f32, held: f32) -> f32 {
        if reduced() || t <= 0.0 || t >= held {
            return 0.0;
        }
        let smooth = |x: f32| {
            let x = x.clamp(0.0, 1.0);
            x * x * (3.0 - 2.0 * x)
        };
        let envelope = smooth(t / LYRIC_WAVE_IN) * smooth((held - t) / LYRIC_WAVE_OUT);
        let cycle = t / LYRIC_WAVE_PERIOD - letter as f32 * LYRIC_WAVE_PHASE;
        LYRIC_WAVE_HEIGHT * envelope * (0.5 - 0.5 * (std::f32::consts::TAU * cycle).cos())
    }

    /// 0 to 1.
    pub fn progress(elapsed: f32, secs: f32) -> f32 {
        if reduced() || secs <= 0.0 { 1.0 } else { (elapsed / secs).clamp(0.0, 1.0) }
    }

    /// The fraction of the remaining distance a value following its goal with time constant `secs`
    /// covers in `dt`.
    pub fn follow(dt: f32, secs: f32) -> f32 {
        if reduced() || secs <= 0.0 { 1.0 } else { 1.0 - (-dt / secs).exp() }
    }

    /// `HOVER_EASE` at linear progress `p`, solving the cubic-bezier for x with Newton's method.
    pub fn hover_ease(p: f32) -> f32 {
        cubic_bezier(HOVER_EASE, p)
    }

    /// A CSS `cubic-bezier(x1, y1, x2, y2)` timing function at `p`.
    pub fn cubic_bezier([x1, y1, x2, y2]: [f32; 4], p: f32) -> f32 {
        let p = p.clamp(0.0, 1.0);
        if p <= 0.0 || p >= 1.0 {
            return p;
        }
        let curve = |a: f32, b: f32, s: f32| 3.0 * a * s * (1.0 - s) * (1.0 - s) + 3.0 * b * s * s * (1.0 - s) + s * s * s;
        let slope = |a: f32, b: f32, s: f32| 3.0 * a * (1.0 - s) * (1.0 - s) + 6.0 * (b - a) * s * (1.0 - s) + 3.0 * (1.0 - b) * s * s;
        let mut s = p;
        for _ in 0..8 {
            let dx = slope(x1, x2, s);
            if dx.abs() < 1e-6 {
                break;
            }
            s = (s - (curve(x1, x2, s) - p) / dx).clamp(0.0, 1.0);
        }
        curve(y1, y2, s)
    }

    pub fn duration(secs: f32) -> Duration {
        Duration::from_secs_f32(secs)
    }

    /// Play and skip's hover scale. They don't grow while motion is reduced.
    pub fn hover_grow() -> f32 {
        if reduced() { 1.0 } else { super::size::HOVER_GROW }
    }
}

/// a player control's press: `Held` while down; let go, `Squash`, then `Over`, then `Rest`, one step after another.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PressPhase {
    Rest,
    Held,
    Squash,
    Over,
}

impl PressPhase {
    pub fn scale(self) -> f32 {
        match self {
            PressPhase::Rest => 1.0,
            PressPhase::Held => size::PRESS_HOLD,
            PressPhase::Squash => size::PRESS_SQUASH,
            PressPhase::Over => size::PRESS_OVER,
        }
    }

    /// seconds and curve of the move into this phase.
    pub fn motion(self) -> (f32, [f32; 4]) {
        match self {
            PressPhase::Held | PressPhase::Squash => (motion::PRESS_IN, motion::EASE_OUT),
            PressPhase::Over | PressPhase::Rest => (motion::PRESS_OUT, motion::EASE_IN_OUT),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    /// Interface size as a fraction, 1.0 at 100%.
    pub zoom: f32,
    pub accent: Srgb,
    pub on_accent: Srgb,
    pub accent_soft: Srgb,
    /// The window is translucent, so the page and sidebar let the blurred desktop through.
    pub translucent: bool,
}

const ON_ACCENT_DARK: u32 = 0x0b1220;

impl Theme {
    pub fn new(accent_rgb: u32, zoom: f32, translucent: bool) -> Self {
        let accent = Srgb::hex(accent_rgb);
        Self { zoom, accent, on_accent: Self::contrast_text(accent), accent_soft: accent.mix(Srgb::hex(0xffffff), 0.55), translucent }
    }

    /// White or the dark ink, whichever has more APCA contrast on the accent.
    fn contrast_text(accent: Srgb) -> Srgb {
        let (dark, white) = (Srgb::hex(ON_ACCENT_DARK), Srgb::hex(0xffffff));
        if apca_lc(white, accent).abs() > apca_lc(dark, accent).abs() { white } else { dark }
    }

    /// A token length on screen. Not rounded, since GPUI snaps to device pixels when painting and
    /// column positions shouldn't accumulate rounding.
    pub fn s(&self, token: f32) -> Pixels {
        px(token * BASE_SCALE * self.zoom)
    }

    /// for converting widths back to tokens.
    pub fn token_scale(&self) -> f32 {
        BASE_SCALE * self.zoom
    }

    /// Text size on screen. `TEXT_MD` is 14px at 100%.
    pub fn text(&self, token: f32) -> Pixels {
        self.s(token)
    }

    /// The page behind everything, letting the desktop through when translucent.
    pub fn bg(&self) -> Rgba {
        let bg = rgb(BG);
        if self.translucent { Rgba { a: BG_WINDOW_ALPHA, ..bg } } else { bg }
    }
    pub fn panel(&self) -> Rgba {
        rgb(PANEL)
    }
    pub fn panel_2(&self) -> Rgba {
        rgb(PANEL_2)
    }
    /// Text and icon colors, at 90%, 68% and 57% white.
    pub fn t1(&self) -> Rgba {
        rgba(0xffffffe6)
    }
    pub fn t2(&self) -> Rgba {
        rgba(0xffffffad)
    }
    pub fn t3(&self) -> Rgba {
        rgba(0xffffff91)
    }
    /// Pure white, for page and item titles and selected items.
    pub fn t_em(&self) -> Rgba {
        rgb(0xffffff)
    }
    /// Sidebar row ink, and section headings.
    pub fn t_nav(&self) -> Rgba {
        self.t_em()
    }
    /// `--t-nav-section`: the sidebar's headings and their chevrons.
    pub fn t_nav_section(&self) -> Rgba {
        self.t_em()
    }
    /// `--cover-mark-ink`.
    pub fn cover_mark_ink(&self) -> Rgba {
        rgba(0xffffffbf)
    }
    /// the Essentials tile's band text, and the paper its band mixes the tint into.
    pub fn essentials_ink(&self) -> Rgba {
        rgb(0x111111)
    }
    pub fn essentials_paper(&self) -> Srgb {
        Srgb::hex(0xc9c9c9)
    }
    /// `--station-ring-ink`.
    pub fn station_ring_ink(&self) -> Rgba {
        rgba(0xffffffb3)
    }
    /// `--t-on-art`, `--knob`, `--on-danger`.
    pub fn t_on_art(&self) -> Hsla {
        gpui::white()
    }
    pub fn knob(&self) -> Hsla {
        gpui::white()
    }
    pub fn on_danger(&self) -> Hsla {
        gpui::white()
    }
    pub fn t_section(&self) -> Rgba {
        self.t3()
    }
    /// The bottom bar's title, ink and track. The bar is neutral, never the cover tint.
    pub fn bar_title(&self) -> Rgba {
        self.t1()
    }
    pub fn bar_ink(&self) -> Rgba {
        self.t2()
    }
    pub fn bar_track(&self) -> Rgba {
        rgba(0xffffff38)
    }
    /// The unplayed track in the dock and expanded player.
    pub fn player_track(&self) -> Rgba {
        rgba(0x00000033)
    }
    /// The player's Lyrics, Artwork and Queue switcher over the cover tint.
    pub fn switcher_glass(&self) -> Rgba {
        rgba(0xffffff1f)
    }
    pub fn row(&self) -> Rgba {
        rgba(0xffffff12)
    }
    pub fn hover(&self) -> Rgba {
        rgba(0xffffff1a)
    }
    /// What a small control's ink brightens to on hover. The tinted player uses `player_fg` instead.
    pub fn hover_ink(&self) -> Hsla {
        self.t1().into()
    }
    /// A hovered button's fill while pressed.
    pub fn press(&self) -> Rgba {
        rgba(0xffffff29)
    }
    /// A downloaded collection's filled mark, and a download ring as it fills.
    pub fn downloaded(&self) -> Hsla {
        self.accent_hsla()
    }
    /// The arrow on the filled download mark.
    pub fn downloaded_ink(&self) -> Hsla {
        self.on_accent.hsla(1.0)
    }
    /// The ring a download fills.
    pub fn download_track(&self) -> Rgba {
        rgba(0xffffff38)
    }
    /// A sync's bar, the accent over the download track.
    pub fn sync_fill(&self) -> Hsla {
        self.accent_hsla()
    }
    pub fn sync_track(&self) -> Rgba {
        self.download_track()
    }
    /// The ring and arrow of a waiting download, such as when metered, offline or queued.
    pub fn download_paused(&self) -> Rgba {
        self.t3()
    }
    pub fn line(&self) -> Rgba {
        rgba(0xffffff14)
    }
    /// `--active-fill`.
    pub fn active_fill(&self) -> Rgba {
        rgba(ACTIVE_FILL)
    }
    pub fn glass_on(&self) -> Rgba {
        rgba(0xffffff1f)
    }
    /// `--segmented-track`, `--segmented-divider`.
    pub fn segmented_track(&self) -> Rgba {
        rgba(0xffffff14)
    }
    pub fn segmented_divider(&self) -> Rgba {
        rgba(0xffffff40)
    }
    /// `--chip-on`, `--on-chip`.
    pub fn chip_on(&self) -> Rgba {
        rgb(0xffffff)
    }
    pub fn on_chip(&self) -> Rgba {
        rgb(0x111111)
    }
    /// A glass button's fill while hovered.
    pub fn glass_hover(&self) -> Rgba {
        rgba(0x5c5c6285)
    }
    /// A glass control's fill while pressed or open.
    pub fn glass_press(&self) -> Rgba {
        rgba(0x70707694)
    }
    /// The 1px divider between controls inside a glass pill.
    pub fn glass_divider(&self) -> Rgba {
        rgba(0xffffff38)
    }
    /// The surface of every floating panel, over the float blur. `float_fill` stands in without
    /// backdrop blur.
    pub fn float_glass(&self) -> Rgba {
        rgba(0x26262ad1)
    }
    pub fn float_fill(&self) -> Rgba {
        rgba(0x26262af5)
    }
    /// `--menu-glass`: menus and popovers, a light veil so what they float over shows through the blur.
    pub fn menu_glass(&self) -> Rgba {
        rgba(0x5f5f6469)
    }
    /// `--dialog-glass`; `dialog_fill` without blur.
    pub fn dialog_glass(&self) -> Rgba {
        rgba(0x26262ae0)
    }
    pub fn dialog_fill(&self) -> Rgba {
        rgba(0x26262afa)
    }
    /// The scrim behind a dialog and the sidebar drawer.
    pub fn scrim(&self) -> Rgba {
        rgba(0x00000073)
    }
    /// `--countdown-fill`.
    pub fn countdown_fill(&self) -> Rgba {
        rgba(0x0000003d)
    }
    /// the line between countdown cells.
    pub fn countdown_divider(&self) -> Rgba {
        self.glass_divider()
    }
    /// `--row-selected`.
    pub fn row_selected(&self) -> Hsla {
        self.accent.hsla(0.3)
    }
    /// `--row-flash-fill`.
    pub fn row_flash_fill(&self) -> Hsla {
        self.accent.hsla(0.24)
    }
    /// `--drag-slot`, `--drag-slot-edge`, `--drag-card`, `--drag-card-edge`, `--drag-card-under`, `--drag-card-under-edge`.
    pub fn drag_slot(&self) -> Rgba {
        rgba(0xffffff0d)
    }
    pub fn drag_slot_edge(&self) -> Rgba {
        rgba(0xffffff12)
    }
    pub fn drag_card(&self) -> Rgba {
        rgb(PANEL_2)
    }
    pub fn drag_card_edge(&self) -> Rgba {
        rgba(0xffffff1f)
    }
    pub fn drag_card_under(&self) -> Rgba {
        rgb(0x34343a)
    }
    pub fn drag_card_under_edge(&self) -> Rgba {
        rgba(0xffffff14)
    }
    /// A drop target under dragged songs, filled with the accent at 24% and ringed in the accent.
    pub fn drop_target(&self) -> Hsla {
        self.accent.hsla(0.24)
    }
    pub fn drop_target_edge(&self) -> Hsla {
        self.accent_hsla()
    }
    /// A queue row hovered or selected over the tinted player.
    pub fn on_art_hover(&self) -> Rgba {
        rgba(0xffffff1f)
    }
    pub fn on_art_selected(&self) -> Rgba {
        rgba(0xffffff38)
    }
    /// Where artwork hasn't arrived yet, over the player or the bar's glass.
    pub fn on_art_placeholder(&self) -> Rgba {
        rgba(0xffffff14)
    }
    /// The player switcher's selected segment and its edge.
    pub fn switcher_on(&self) -> Rgba {
        rgba(0xffffff33)
    }
    pub fn switcher_edge(&self) -> Rgba {
        rgba(0xffffff38)
    }
    /// The panel glass of the sidebar and the Settings and Equalizer cards, so page tints reach
    /// behind it without a hard edge. When translucent it's a white overlay letting through as much
    /// of the blurred desktop as the page.
    pub fn panel_glass(&self) -> Rgba {
        if self.translucent { rgba(0xffffff07) } else { rgba(0x1f1f21b3) }
    }
    /// The panel glass for pages whose tint or glow reaches behind the sidebar. A white lift over a
    /// bright photo washes out the text, so these always get the dark glass.
    pub fn sidebar_glass(&self) -> Rgba {
        rgba(0x1f1f21b3)
    }
    /// `--glass` over `--bg`, nearly opaque so scrolled rows stay unreadable behind pills.
    pub fn glass_fill(&self) -> Rgba {
        rgba(0x27272afa)
    }
    /// `--glass`, for pills that frost what passes under them.
    pub fn glass(&self) -> Rgba {
        rgba(0x3a3a3e75)
    }
    /// The bar glass over the page, for renderers without backdrop blur.
    pub fn bar_fill(&self) -> Rgba {
        rgba(0x2f2f33fa)
    }
    /// `--bar-glass`, over the bar's backdrop blur.
    pub fn bar_glass(&self) -> Rgba {
        rgba(0x2e2e32b3)
    }
    pub fn accent_hsla(&self) -> Hsla {
        self.accent.hsla(1.0)
    }
    pub fn accent_soft_hsla(&self) -> Hsla {
        self.accent_soft.hsla(1.0)
    }
    /// `--link-ink`: a web address inside running text.
    pub fn link_ink(&self) -> Hsla {
        self.accent_soft_hsla()
    }
    /// `--popularity-on`, `--popularity-off` (`--t1` at 34%), `--popularity-heat-foot`, `--popularity-heat-top`; `--popularity-on-now`: the playing row's lit bars.
    pub fn popularity_on(&self) -> Hsla {
        self.t1().into()
    }
    pub fn popularity_off(&self) -> Hsla {
        let mut c: Hsla = self.t1().into();
        c.a *= 0.34;
        c
    }
    pub fn popularity_on_now(&self) -> Hsla {
        self.accent_soft_hsla()
    }
    pub fn popularity_heat_foot(&self) -> Hsla {
        self.t2().into()
    }
    pub fn popularity_heat_top(&self) -> Hsla {
        gpui::white()
    }
}
