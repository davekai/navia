## General

- Rust isn't formatted with `cargo fmt`. Match the surrounding code and don't reformat files you touch.
- Don't edit generated files. Regenerate them:
  - icons, the installer splash and `nv-brand-*.svg`: `python3 -I tools/brand_icons.py`
  - `apps/navia-apple/Navia/Resources/Locales/`: `python3 -I tools/i18n.py sync`
  - the Swift bindings: `apps/navia-apple/scripts/build-core.sh`
- Change GPUI only through a patch in `vendor/gpui/patches/`. `vendor/gpui/zed` and `wgpu` are downloaded.
- No useless comments. Comment only what the code can't say on its own (why, a unit, a platform quirk), in one short plain line. Never restate the code or narrate its history.
- Code, comments, tests and commit messages never mention other music apps or clients, streaming services, or a chat with an assistant. Describe the behaviour itself. Services Navia actually talks to (the server, Deezer, Discord, AirPlay) are named where the code calls them.
- Commit messages use Conventional Commits with a scope: see CONTRIBUTING.md, "Pull requests".

## Commands

```sh
vendor/gpui/fetch-gpui.sh   # once, and after changing the GPUI revision or a patch
cargo run --release -p navia-gpui
cargo test -p navia-core
cargo test -p navia-integrations
cargo xwin check --target x86_64-pc-windows-msvc -p navia-gpui
python3 -I tools/i18n.py check
```

The Mac app and packaging: read `docs/building.md`.

## Architecture

Shared behaviour goes in `crates/navia-core`; the apps only draw it. Read `docs/architecture.md` when crossing
crates, the FFI or the apps.

## Design system

- Colors, sizes, radii, shadows, glass, motion and fonts come from the tokens and shared components:
  `apps/navia-gpui/src/theme.rs` and `ui.rs`, `apps/navia-apple/Navia/Theme/Tokens.swift` and `Views/Controls.swift`.
- No hard-coded values in a view. Add a token, or a variant of the shared component.
- Change both apps together, with the same token names.
- Remove what your change leaves unused.

## Server

Navia is a Navidrome client first, then OpenSubsonic, then plain Subsonic.

- Build for Navidrome. Use its native API (`crates/navia-core/src/client/native.rs`) where it does more than
  Subsonic, and check Navidrome's docs and version before relying on an endpoint or field.
- Use OpenSubsonic extensions when the server offers them, and fall back to plain Subsonic when it doesn't.
- Only show what the server's data supports. Don't invent rows, labels or features.
- Detect what the server supports and work without it. Never assume a setting, plugin or library size.
- Tests and tools never write to a server: playlists, favorites, ratings, scrobbles or the play queue. Run the app
  with `NAVIA_READ_ONLY=1` against a server other people use.
- Discord and Deezer stay off until the user turns them on. Scrobbling is on by default for a new account and follows Private mode.

## Audio

- Prove playback changes (gapless, crossfade, seeking) with measurements, not by ear.
- Read `docs/autoplay.md` before changing Autoplay or recommendations.

## Translations

All visible text goes through `tr`, `trf` and `trn` in GPUI, or `L` and `Ln` in Swift, keyed by the English text.
After adding text, run `tools/i18n.py extract`, translate the new keys in `locales/*.json`, then run
`tools/i18n.py sync` and `tools/i18n.py check`.
