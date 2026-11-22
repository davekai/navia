# Architecture

Navia is a Rust core with two native apps on top. The core does the work: the server, the local library,
playback, recommendations and system integration. The apps draw the interface.

The GPUI app calls the core directly. The Mac app reaches it through UniFFI bindings in `crates/navia-ffi`.

## Crates

### `crates/navia-core`

- `client/`: talks to the server. Navia is a Navidrome client first, then OpenSubsonic, then plain Subsonic.
  `native.rs` uses Navidrome's own API for sync counts, discography, scrobble history, playlist images and smart
  playlist rules. The rest is the OpenSubsonic API, with extensions when the server has them. `capabilities`
  records what the server supports. `form.rs` sends the credentials in a POST body instead of the URL when the
  server offers OpenSubsonic `formPost`.
- `library/`: the local SQLite database and its FTS5 search index. The first sync pages through the whole
  library. After that, a sync fetches only what changed on the server, and background refreshes pick up stars,
  play counts, playlists and new albums. Browse pages, genres, discography, Home rows and the album page's
  copyright line are queries over this database.
- `audio/`: the playback engine. Symphonia decodes, with libopus for Opus. `rubato` resamples and `cpal` plays.
  Gapless and crossfade are sample-accurate. AAC gapless reads MP4 edit lists and iTunSMPB directly. Songs stream
  over HTTP range requests, and formats Navia can't decode are transcoded by the server. ReplayGain, the
  equalizer, internet radio (`live.rs`) and AirPlay output (`airplay/`) are here too. The Mix transition measures
  each song's first and last seconds once (`edges.rs`) and plans every join from them (`transition.rs`): its
  length, beat alignment and loudness. Beats and tempo come from aubio, in `crates/navia-aubio`.
- `play_queue`, `queue_order`, `queue_sync`: the queue, shuffle and repeat, and saving the queue to the server so
  another device can resume it.
- `engine/`, `autoplay`, `recommend`, `artist_radio`: the recommendation engine behind Autoplay, song and artist
  radio, and mixes. See [autoplay.md](autoplay.md). `deezer` is a cached, rate-limited client for Deezer's public
  API, used only when the user turns it on. `listenbrainz` asks ListenBrainz for similar songs under the same
  switch. `upcoming` (announced albums by library artists) and `album_heat` (each album song's popularity) also
  come from Deezer.
- `reporting`: scrobbles and now playing. Reports made offline are sent later.
- `downloads`, `songcache`: offline downloads, and the cache of played songs.
- `artwork`, `covers`: cover art on disk, animated covers (`artwork/animated.rs`), and the ready-made playlist
  covers the New Playlist dialog uploads.
- `smart_editor`, `smart_rules`, `song_filters`, `artist_filters`: the smart playlist editor and the filters.
  Their labels are translation keys, so both apps share them.
- `listen_along`, `lanyard/`: listening along with someone on the same server, or with a friend's Spotify
  through Lanyard.
- `session`, `connect`: signing in and checking a server address.

### `crates/navia-integrations`

Desktop integrations: MPRIS on Linux (`zbus`), media controls on Windows (SMTC through `souvlaki`), Discord Rich
Presence, cover art lookup for Discord, and metered connection detection. They read a `PlayerSnapshot` and send
commands back through a trait, so the player doesn't depend on them. The Mac app handles Now Playing itself.

### `crates/navia-ffi`

UniFFI bindings that expose the core to Swift. `build-xcframework.sh` builds them as an xcframework. It isn't a
workspace member, since it only builds for Apple targets.

### `crates/navia-aubio`

aubio's beat tracker, tempo estimate and onset detector, built from the vendored C source in `aubio/` and linked
statically.

### `crates/airplay`

AirPlay 2 protocol code (pairing, RTSP, timing, ALAC and RTP), trimmed from airplay2-rs. Navia's sender is in
`navia-core`'s `audio::airplay`.

## Apps

### `apps/navia-gpui` (Linux and Windows)

Built on GPUI from a pinned Zed revision. `vendor/gpui/fetch-gpui.sh` downloads it and applies Navia's patches.
See [vendor/gpui/README.md](../vendor/gpui/README.md). On Linux it runs on Wayland only.

- `theme.rs` has the design tokens. `ui.rs`, `ui_controls.rs` and `ui_scroll.rs` have the shared components that
  every view (`view_*.rs`) is built from.
- `shell.rs` and `shell_data.rs` hold app state and navigation. `core_library.rs` and `core_player.rs` connect to
  the core.
- `i18n.rs` loads the translations from `locales/`.

### `apps/navia-apple` (macOS)

SwiftUI, with AppKit for very large song lists (`NSTableView`) and menus. It links the core through
`NaviaCoreKit`, a local Swift package around the xcframework from `scripts/build-core.sh`.

- `Theme/Tokens.swift` has the design tokens, with the same names as GPUI's `theme.rs`.
- `Views/Controls.swift` and the other shared views are the components.
- `Model/` has the view models. `SampleModels.swift` runs the app without a server.

## Shared

- **Design tokens:** colors, sizes, radii, shadows, glass and motion, with the same names in `theme.rs` and
  `Tokens.swift`.
- **Translations:** `locales/`, keyed by the English text, with CLDR plurals. `tools/i18n.py` manages them and
  copies them into the Mac app.
- **Brand:** `assets/brand/`. `tools/brand_icons.py` generates every icon from it.
- **Version:** set once in the root `Cargo.toml` and read by `tools/version.sh`.

## Where data is stored

The library, caches and downloads go in the platform's data folder: XDG on Linux, Application Support on macOS
and `%LocalAppData%\NaviaData` on Windows. Settings go in the config folder. Passwords are kept in the system
keyring: Secret Service on Linux, the Keychain on macOS and Credential Manager on Windows.
