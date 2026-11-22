<p align="center">
  <img alt="Navia" src="assets/brand/navia-app-icon-dark.svg" width="128">
</p>

<h3 align="center">Navia</h3>

<p align="center">A native music client for <a href="https://www.navidrome.org/">Navidrome</a>.</p>
<p align="center">Linux, macOS and Windows.</p>

<p align="center">
  <a href="https://github.com/davekai/navia/releases/latest"><img alt="Download for Linux" src="docs/readme/download-linux.svg"></a>
  <a href="https://github.com/davekai/navia/releases/latest"><img alt="Download for macOS" src="docs/readme/download-macos.svg"></a>
  <a href="https://github.com/davekai/navia/releases/latest"><img alt="Download for Windows" src="docs/readme/download-windows.svg"></a>
</p>

<p align="center">
  <img alt="Navia on macOS: Home with Artist Radio and On Repeat, and the side player" src="docs/screenshots/home.webp">
</p>

<table>
  <tr>
    <td><img alt="Listening along with a friend's Spotify" src="docs/screenshots/listen-along.webp"></td>
    <td><img alt="Synced lyrics in the full-screen player" src="docs/screenshots/lyrics.webp"></td>
  </tr>
  <tr>
    <td><img alt="The queue with Autoplay's picks" src="docs/screenshots/queue.webp"></td>
    <td><img alt="Navia on Linux (KDE Plasma) with synced lyrics in the side player" src="docs/screenshots/linux-home.webp"></td>
  </tr>
</table>

<p align="center">
  <img alt="The mini player" src="docs/screenshots/mini-players.webp" width="640">
</p>

## Features

- **Listen along**: two ways, in sync. With someone on the same Navidrome server, or with a friend's Spotify through Discord.
- **Custom Autoplay**: when your music ends, Navia keeps going with songs that match what you were playing, learned from your library and listening stored locally, with Deezer integration.
- **AirPlay**: play to AirPlay 2 TVs and speakers including Linux and Windows.
- **Mini player**: a compact player window.
- **Gapless and crossfade**: gapless playback, crossfade, and a smart mode that keeps albums seamless.
- **Synced lyrics**.
- **Fuzzy search**, typo-tolerant.
- **Smart playlists** with a rule editor.
- **Offline**: download songs, albums and playlists, along with song caching.
- **Discord Rich Presence**: show what you're listening to on Discord.

## Install

Download the latest version from [Releases](../../releases).

- **Windows**: installer, with automatic updates.
- **macOS**: `.dmg`, for Apple Silicon and Intel.
- **Linux**: `.rpm`, `.pkg.tar.zst` (Arch) and `.deb`. Flatpak, AppImage and the AUR are coming soon.

### Fedora

Navia is in [COPR](https://copr.fedorainfracloud.org/coprs/davekai/navia/), so `dnf` keeps it up to date:

```sh
sudo dnf copr enable davekai/navia
sudo dnf install navia
```

## Build from source

See [docs/building.md](docs/building.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[GPL-3.0](LICENSE)
