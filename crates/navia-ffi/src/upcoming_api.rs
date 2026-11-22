//! `navia_core::upcoming` for the Mac app.

use navia_core::deezer::Deezer;
use navia_core::upcoming::{self, PreviewTrack, UpcomingAlbum, UpcomingPage};

use crate::core_api::NaviaCore;
use crate::{SongRow, song_row};

#[derive(uniffi::Record, Clone, Debug)]
pub struct UpcomingTrackRow {
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub duration_seconds: u64,
    pub explicit: bool,
    /// the library's own copy, which plays; without one the row can't.
    pub song: Option<SongRow>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct UpcomingAlbumRow {
    /// Deezer's album id.
    pub id: u64,
    pub title: String,
    pub artist: String,
    /// the library artist it was found for.
    pub artist_id: String,
    /// never stored (Deezer's terms).
    pub cover_url: Option<String>,
    /// `YYYY-MM-DD`.
    pub release_date: String,
    /// local midnight of the release day, Unix seconds.
    pub release_moment: i64,
    /// songs announced in all; `tracks` are those named so far, filled only on the page.
    pub total: u32,
    pub explicit: bool,
    pub tracks: Vec<UpcomingTrackRow>,
}

#[derive(uniffi::Enum, Clone, Debug)]
pub enum UpcomingPageRow {
    Upcoming { album: UpcomingAlbumRow },
    /// the server's copy.
    OnServer { album_id: String },
}

fn album_row(album: UpcomingAlbum, tracks: Vec<PreviewTrack>) -> UpcomingAlbumRow {
    UpcomingAlbumRow {
        id: album.id,
        release_moment: upcoming::release_moment(&album.release_date).unwrap_or_default(),
        title: album.title,
        artist: album.artist,
        artist_id: album.artist_id,
        cover_url: upcoming::cover_url(&album.cover, COVER_PX),
        release_date: album.release_date,
        total: album.total,
        explicit: album.explicit,
        tracks: tracks
            .into_iter()
            .map(|p| UpcomingTrackRow { id: p.track.id, title: p.track.title, artist: p.track.artist, duration_seconds: p.track.duration_s, explicit: p.track.explicit, song: p.song.map(|(r, s)| song_row(Some(r), s)) })
            .collect(),
    }
}

/// the header art at 2x.
const COVER_PX: u32 = 500;

fn rows(albums: Vec<UpcomingAlbum>) -> Vec<UpcomingAlbumRow> {
    albums.into_iter().map(|a| album_row(a, Vec::new())).collect()
}

#[uniffi::export(async_runtime = "tokio")]
impl NaviaCore {
    /// test runs only read what's kept.
    pub async fn upcoming_for_artist(&self, artist_id: String) -> Vec<UpcomingAlbumRow> {
        let (client, db, ask, barcode) = (self.client.clone(), self.db.clone(), self.external_allowed(), !self.test_run);
        let found = self.on_runtime(async move { upcoming::for_artist(barcode.then_some(&client), &db, Deezer::shared(), &artist_id, ask).await }).await.unwrap_or_default();
        rows(found)
    }

    pub async fn upcoming_home(&self) -> Vec<UpcomingAlbumRow> {
        let (client, db, ask, barcode) = (self.client.clone(), self.db.clone(), self.external_allowed(), !self.test_run);
        let found = self.on_runtime(async move { upcoming::for_top_artists(barcode.then_some(&client), &db, Deezer::shared(), ask).await }).await.unwrap_or_default();
        rows(found)
    }

    /// never asks Deezer.
    pub fn upcoming_matching(&self, query: String) -> Vec<UpcomingAlbumRow> {
        rows(upcoming::kept_matching(&self.db, &query))
    }

    pub async fn upcoming_page(&self, id: u64) -> Option<UpcomingPageRow> {
        let (client, db, ask, barcode) = (self.client.clone(), self.db.clone(), self.external_allowed(), !self.test_run);
        let page = self.on_runtime(async move { upcoming::page(barcode.then_some(&client), &db, Deezer::shared(), id, ask).await }).await.ok().flatten()?;
        Some(match page {
            UpcomingPage::Upcoming { album, tracks } => UpcomingPageRow::Upcoming { album: album_row(album, tracks) },
            UpcomingPage::OnServer(album_id) => UpcomingPageRow::OnServer { album_id },
        })
    }
}
