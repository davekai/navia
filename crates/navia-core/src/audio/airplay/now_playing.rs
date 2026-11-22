//! Song details and cover art for a receiver's screen, from the server (read-only calls).

use super::backend::{NowPlayingSource, TrackInfo};
use crate::client::Client;

/// Fetches song details and cover art. Runs on the AirPlay runtime's blocking pool.
pub struct ClientNowPlaying {
    client: Client,
}

impl ClientNowPlaying {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

impl NowPlayingSource for ClientNowPlaying {
    fn track(&self, song_id: &str) -> Option<TrackInfo> {
        let handle = tokio::runtime::Handle::try_current().ok()?;
        let song = handle.block_on(self.client.song(song_id)).ok()?;
        Some(TrackInfo {
            title: song.title,
            artist: song.artist.unwrap_or_default(),
            album: song.album.unwrap_or_default(),
            duration_ms: song.duration.and_then(|d| u64::try_from(d).ok()).map(|s| s * 1000),
            cover: song.cover_art,
        })
    }

    fn artwork(&self, cover_id: &str) -> Option<Vec<u8>> {
        let handle = tokio::runtime::Handle::try_current().ok()?;
        let url = self.client.cover_art_url(cover_id, Some(800)).ok()?;
        handle.block_on(async {
            let resp = self.client.fetch(&url).await.send().await.ok()?.error_for_status().ok()?;
            resp.bytes().await.ok().map(|b| b.to_vec())
        })
    }
}
