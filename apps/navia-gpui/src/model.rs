//! A small view-model interface that knows nothing about Navidrome. `core_library.rs`
//! implements it over `navia-core`, and `sample.rs` provides sample data.

use std::sync::Arc;

use gpui::SharedString;

pub use navia_core::library::discography::ReleaseSection;

/// An artist's albums by section, as indexes into `Library::albums`.
pub type Discography = Vec<(ReleaseSection, Vec<u32>)>;

/// "More by <album artist>" under an album. The artist and their other albums, newest first, as
/// indexes, and the discography section "See Discography" opens.
#[derive(Clone, Debug)]
pub struct MoreBy {
    pub artist: u32,
    pub albums: Vec<u32>,
    pub section: ReleaseSection,
}

#[derive(Clone, Debug, Default)]
pub struct Song {
    pub id: SharedString,
    pub title: SharedString,
    pub artist: SharedString,
    pub album: SharedString,
    pub album_id: SharedString,
    pub genre: SharedString,
    pub year: u32,
    pub duration_s: u32,
    pub plays: u32,
    pub starred: bool,
    /// Whether the song is downloaded, which highlights its row.
    pub downloaded: bool,
    pub has_lyrics: bool,
    pub artists: Vec<SharedString>,
    /// Cover ids of artist photos, in the same order as `artists`. Empty without photos.
    pub artist_art: Vec<SharedString>,
    /// The server flags it explicit (OpenSubsonic `explicitStatus`).
    pub explicit: bool,
    /// When it was added to the library, in Unix seconds. 0 when unknown.
    pub added: i64,
    /// Its disc on the album. 0 when the server doesn't say.
    pub disc: u32,
}

#[derive(Clone, Debug)]
pub struct LyricLine {
    pub start_ms: u32,
    pub text: SharedString,
    /// False when the server's lyrics have no timing, so every line has `start_ms` 0.
    pub synced: bool,
    /// The core's line when the server gave real word timing, for the word fill. `None` estimates
    /// it from the line's length.
    pub timed: Option<std::sync::Arc<navia_core::lyrics::LyricLine>>,
    /// the line's bracketed backing vocals, shown smaller under it while current.
    pub backing: Option<SharedString>,
    /// the bracketed part comes before the main words, so it's sung, and filled, first.
    pub backing_first: bool,
}

#[derive(Clone, Debug)]
pub struct PlaylistSummary {
    pub id: SharedString,
    pub name: SharedString,
    /// The owner's username, empty when unknown.
    pub owner: SharedString,
    /// The server says this user can't change it, because it isn't theirs or it's smart.
    pub readonly: bool,
    /// A smart playlist, whose contents the server computes.
    pub smart: bool,
    /// Others on the server can see it.
    pub public: bool,
    /// The signed-in user owns it, or the owner is unknown. Admin rights don't count.
    pub mine: bool,
}

/// Where a playlist is listed, by its owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    /// Yours, private or public, or a playlist whose owner is unknown.
    Mine,
    /// Someone else's public playlist.
    Shared,
    /// Someone else's private playlist, which only an admin sees. Listed only under All.
    Other,
}

impl Section {
    pub fn of(mine: bool, public: bool) -> Self {
        if mine {
            Section::Mine
        } else if public {
            Section::Shared
        } else {
            Section::Other
        }
    }
}

impl PlaylistSummary {
    pub fn section(&self) -> Section {
        Section::of(self.mine, self.public)
    }
}

/// Navidrome compares usernames without regard to case.
pub fn same_user(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Whether `owner` is `me` for sectioning. An unknown owner counts as yours.
pub fn owner_is(owner: Option<&str>, me: &str) -> bool {
    owner.is_none_or(|o| o.is_empty() || same_user(o, me))
}

/// Who is signed in, so the UI can show edit controls only where they'd work.
#[derive(Clone, Debug)]
pub struct Account {
    pub username: SharedString,
    /// Admins can edit playlists they don't own. `None` until the server answers.
    pub admin: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct Album {
    pub id: SharedString,
    pub name: SharedString,
    pub artist: SharedString,
    pub artist_id: Option<SharedString>,
    pub year: u32,
    pub song_count: u32,
    pub duration_s: u32,
    pub genre: SharedString,
    /// When it was added to the library, in Unix seconds.
    pub added: i64,
    pub plays: u32,
    /// Last play by anyone, in Unix seconds. 0 if never played.
    pub last_played: i64,
    pub starred: bool,
    /// The server flags it explicit (OpenSubsonic `explicitStatus`).
    pub explicit: bool,
    /// Discs with their own names, from OpenSubsonic `discTitles`. Unnamed discs are left out.
    pub disc_titles: Vec<(u32, SharedString)>,
    /// The album's version as a label, like "Deluxe Edition". Never explicit or clean.
    pub edition: Option<SharedString>,
    /// Another album by the same artist has the same name, so cards show the edition to tell them apart.
    pub edition_on_card: bool,
    /// days since 1970, full dates only.
    pub release_day: Option<i64>,
}

impl Album {
    /// `today` in days since 1970.
    pub fn is_new_release(&self, today: i64) -> bool {
        self.release_day.is_some_and(|day| (0..navia_core::upcoming::NEW_RELEASE_DAYS).contains(&(today - day)))
    }
}

/// `navia_core::upcoming::UpcomingAlbum` for the shell.
#[derive(Clone, Debug, PartialEq)]
pub struct Upcoming {
    /// Deezer's album id.
    pub id: u64,
    pub title: SharedString,
    pub artist: SharedString,
    /// the library artist it was found for.
    pub artist_id: SharedString,
    /// the cover's key for `album_thumbnail`.
    pub cover: SharedString,
    /// `YYYY-MM-DD`.
    pub release_date: SharedString,
    /// local midnight of the release day, Unix seconds.
    pub release_moment: i64,
    /// songs announced in all.
    pub total: u32,
    pub explicit: bool,
}

/// `song` is the library's copy; without one it can't play.
#[derive(Clone, Debug, PartialEq)]
pub struct UpcomingTrack {
    pub title: SharedString,
    pub artist: SharedString,
    pub duration_s: u32,
    pub explicit: bool,
    pub song: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UpcomingView {
    Loading,
    Missing,
    Upcoming(Upcoming, Vec<UpcomingTrack>),
    /// the server has it now: this library album's id.
    OnServer(SharedString),
}

#[derive(Clone, Debug)]
pub struct Artist {
    pub id: SharedString,
    pub name: SharedString,
    /// Cover id of the artist's own photo, for `album_thumbnail`. `None` shows the plain fallback.
    pub image: Option<SharedString>,
    pub plays: u32,
    pub starred: bool,
}

#[derive(Clone, Debug)]
pub struct ArtistInfo {
    pub biography: SharedString,
    pub similar: Vec<SharedString>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Genre {
    pub name: SharedString,
    pub song_count: u32,
}

/// A genre on the Genres page. Its name as the server has it, its album count, and its covers
/// as indexes into `Library::albums`.
#[derive(Clone, Debug, Default)]
pub struct GenreEntry {
    pub name: SharedString,
    pub albums: u32,
    pub covers: Vec<u32>,
}

/// The Genres page. Top genres with up to three unrepeated covers each, every genre as a row,
/// and the remaining genres by album count for Search's tiles.
#[derive(Clone, Debug, Default)]
pub struct GenrePage {
    pub top: Vec<GenreEntry>,
    pub all: Vec<GenreEntry>,
    pub browse: Vec<GenreEntry>,
}

impl GenrePage {
    /// Builds the page from the core's, mapping covers to album indexes with `index` and dropping
    /// any it can't map.
    pub fn from_core(page: navia_core::library::genres::GenrePage, index: impl Fn(&navia_core::library::genres::GenreCover) -> Option<u32>) -> Self {
        let entry = |g: navia_core::library::genres::GenreEntry| GenreEntry { name: g.name.into(), albums: g.albums, covers: g.covers.iter().filter_map(&index).collect() };
        Self { top: page.top.into_iter().map(&entry).collect(), all: page.all.into_iter().map(&entry).collect(), browse: page.browse.into_iter().map(&entry).collect() }
    }
}

#[derive(Clone, Debug)]
pub struct AlbumTrack {
    /// Index into `songs`.
    pub song: u32,
    pub number: Option<u32>,
    pub disc: u32,
    pub hi_res: bool,
}

#[derive(Clone, Debug)]
pub struct PlaylistDetails {
    pub id: SharedString,
    pub name: SharedString,
    pub song_count: u32,
    pub duration_s: u32,
    pub description: SharedString,
    pub smart: bool,
    pub public: bool,
    /// Whether this user can edit the playlist or add to it. True for owners and admins.
    pub owned: bool,
    /// The signed-in user owns it, or the owner is unknown. Used for sections and Add to Playlist.
    pub mine: bool,
    /// The owner's name, for playlists that aren't yours. Empty when unknown.
    pub owner: SharedString,
    /// The smart playlist's rules, as Navidrome stores them.
    pub rules: Option<String>,
    /// The server says this user can't change its songs, or didn't say.
    pub readonly: bool,
}

impl PlaylistDetails {
    pub fn section(&self) -> Section {
        Section::of(self.mine, self.public)
    }
}

/// A playlist's default cover. The server's, either uploaded or its own grid, or a grid of the
/// playlist's album covers when the server has none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaylistCover {
    /// Encoded image from the server.
    Server(Vec<u8>),
    /// Up to 4 distinct album ids to tile.
    Albums(Vec<SharedString>),
    /// Nothing to show, so a plain placeholder.
    Empty,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RadioStation {
    pub id: SharedString,
    pub name: SharedString,
    pub url: SharedString,
}

/// The Home rows that follow recent taste. Artists and albums are indexes into the library's
/// lists, and songs are row ids.
#[derive(Clone, Debug, Default)]
pub struct HomeTaste {
    /// Artists with real plays, best first.
    pub artists: Vec<u32>,
    /// Albums with real plays, best first.
    pub albums: Vec<u32>,
    /// Today's "World of" artist.
    pub world_of: Option<u32>,
    pub on_repeat: Vec<u32>,
    pub back_in_rotation: Vec<u32>,
    /// "Because you've been playing <seed>", with the seed and similar artists.
    pub because: Option<(u32, Vec<u32>)>,
    /// Artist Radio stations, each a seed and its neighbors with photos, best seed first.
    pub stations: Vec<(u32, Vec<u32>)>,
    /// Essentials: played artists with enough top songs, best lately first.
    pub essentials: Vec<u32>,
}

/// One of today's mixes on For You.
#[derive(Clone, Debug, Default)]
pub struct MixInfo {
    /// The mix page's address, `artist:<id>` or `genre:<name>`.
    pub id: SharedString,
    pub genre: bool,
    /// The artist's or genre's name. The title is "<name> Mix".
    pub name: SharedString,
    /// Song row ids, in order.
    pub songs: Vec<u32>,
    pub duration_s: u32,
    /// Main artists in order of first appearance, for the "A, B, C and more" line.
    pub artists: Vec<SharedString>,
    /// The first four different albums, which make its cover like a playlist's grid.
    pub covers: Vec<SharedString>,
}

/// Discover's For You. Albums are indexes into `Library::albums` and songs are row ids. Rows are
/// empty when the server has no data for them.
#[derive(Clone, Debug, Default)]
pub struct ForYouPage {
    pub mixes: Vec<MixInfo>,
    pub new_to_you: Vec<u32>,
    pub unplayed: Vec<u32>,
    pub forgotten: Vec<u32>,
    pub on_this_day: Vec<u32>,
    pub not_played_recently: Vec<u32>,
    /// today's mixes are still being made, so an empty page isn't final.
    pub loading: bool,
}

/// The only data contract the UI needs from a library. The shell owns playback state, and the
/// library answers questions about content. Safe to call from background threads.
pub trait Library: Send + Sync + 'static {
    /// All songs, most played first.
    fn songs(&self) -> Arc<Vec<Song>>;
    /// Index into `songs` of the song to restore as now playing.
    fn now_playing(&self) -> Option<usize>;
    fn playlists(&self) -> Vec<PlaylistSummary>;
    /// The signed-in account, when there is one.
    fn account(&self) -> Option<Account> {
        None
    }
    /// The key that separates this account's local data from another's.
    fn account_key(&self) -> String {
        "local".to_string()
    }
    /// Encoded cover art for an album.
    fn album_artwork(&self, album_id: &str) -> Option<Vec<u8>>;
    /// Encoded art about `px` pixels on its long side. `id` is an album id or an artist photo id.
    fn album_thumbnail(&self, id: &str, px: u32) -> Option<Vec<u8>> {
        let _ = px;
        self.album_artwork(id)
    }
    /// whether the art last given for `id` was the server's no-cover placeholder, to be asked for
    /// again after `navia_core::artwork::NO_STORE_TTL` rather than kept.
    fn artwork_is_placeholder(&self, id: &str) -> bool {
        let _ = id;
        false
    }

    /// the frames of an animated cover; `key` is an album id or `pl:<playlist id>`. `None` for a still cover.
    fn animated_cover(&self, key: &str, px: u32) -> Option<navia_core::artwork::AnimatedArt> {
        let _ = (key, px);
        None
    }
    /// The cover for a playlist, about `px` pixels square.
    fn playlist_cover(&self, id: &str, _px: u32) -> PlaylistCover {
        let details = self.playlist_details();
        if !details.iter().any(|p| p.id.as_ref() == id) {
            return PlaylistCover::Empty;
        }
        let songs = self.songs();
        let songs: Vec<&Song> = self.playlist_songs(id).into_iter().filter_map(|i| songs.get(i as usize)).collect();
        match crate::art::first_distinct(songs.iter().map(|s| s.album_id.clone())) {
            albums if albums.is_empty() => PlaylistCover::Empty,
            albums => PlaylistCover::Albums(albums),
        }
    }
    /// Lyric lines in order, empty if the song has none. Every line at 0 means unsynced.
    fn lyrics(&self, song_id: &str) -> Vec<LyricLine>;
    /// lyricists, else composers (OpenSubsonic `contributors`); empty when the server credits neither.
    fn lyrics_writers(&self, _song_id: &str) -> Vec<String> {
        Vec::new()
    }
    /// Every album, most played first. Empty until the library can answer.
    fn albums(&self) -> Arc<Vec<Album>> {
        Arc::default()
    }
    /// Every artist, most played first.
    fn artists(&self) -> Arc<Vec<Artist>> {
        Arc::default()
    }
    fn playlist_details(&self) -> Arc<Vec<PlaylistDetails>> {
        Arc::default()
    }
    /// Genres with the most songs first.
    fn genres(&self) -> Arc<Vec<Genre>> {
        Arc::default()
    }
    /// The Genres page, laid out by the core's rules. Sample data uses each album's genre and
    /// plays in place of recent taste.
    fn genre_page(&self) -> Arc<GenrePage> {
        use navia_core::library::genres::{GenreCover, GenreOverview, genre_page_of, sort_genres};
        let albums = self.albums();
        let mut by_name: std::collections::HashMap<SharedString, GenreOverview> = std::collections::HashMap::new();
        for (i, album) in albums.iter().enumerate() {
            if album.genre.trim().is_empty() {
                continue;
            }
            let g = by_name.entry(album.genre.clone()).or_insert_with(|| GenreOverview { name: album.genre.to_string(), albums: 0, taste: 0.0, taste_fallback: 0.0, played: false, covers: Vec::new() });
            g.albums += 1;
            g.taste += f64::from(album.plays);
            g.played |= album.plays > 0;
            g.covers.push(GenreCover { rowid: i as u32, id: album.id.to_string(), cover_art: album.id.to_string() });
        }
        let mut all: Vec<GenreOverview> = by_name.into_values().collect();
        for g in &mut all {
            g.covers.sort_by_key(|c| { let a = &albums[c.rowid as usize]; (std::cmp::Reverse(a.plays), std::cmp::Reverse(a.added)) });
        }
        sort_genres(&mut all);
        Arc::new(GenrePage::from_core(genre_page_of(&all), |c| Some(c.rowid)))
    }
    fn radio_stations(&self) -> Arc<Vec<RadioStation>> {
        Arc::default()
    }
    /// An album's tracks in disc and track order.
    fn album_tracks(&self, _album_id: &str) -> Vec<AlbumTrack> {
        Vec::new()
    }
    fn album_description(&self, _album_id: &str) -> SharedString {
        SharedString::default()
    }
    /// How many songs the stored copy of a playlist holds, including any not in the library.
    fn playlist_entry_count(&self, _playlist_id: &str) -> usize {
        0
    }
    fn playlist_songs(&self, _playlist_id: &str) -> Vec<u32> {
        Vec::new()
    }
    /// the songs this device already has for a playlist, without asking the server.
    fn stored_playlist_songs(&self, playlist_id: &str) -> Vec<u32> {
        self.playlist_songs(playlist_id)
    }
    /// An artist's albums, as indexes into `albums`, newest first.
    fn artist_albums(&self, _artist_id: &str) -> Vec<u32> {
        Vec::new()
    }
    /// The artist's albums by release type as indexes into `albums`, newest first, without empty
    /// sections. `None` while it's being worked out. Sample data puts everything under Albums.
    fn artist_discography(&self, artist_id: &str) -> Option<Discography> {
        let own = self.artist_albums(artist_id);
        Some(if own.is_empty() { Vec::new() } else { vec![(ReleaseSection::Albums, own)] })
    }
    /// The artist's best-known songs, with duplicates removed, as indexes into `songs`.
    fn artist_top_songs(&self, _artist_id: &str) -> Vec<u32> {
        Vec::new()
    }
    /// All the artist's songs, when there are no top songs to play.
    fn artist_songs(&self, _artist_id: &str) -> Vec<u32> {
        Vec::new()
    }
    /// What's known about an artist's similar songs without asking the server. `None` means the
    /// server hasn't been asked yet, since the first request can take seconds.
    fn similar_known(&self, artist_id: &str) -> Option<Vec<u32>> {
        Some(self.similar_songs(artist_id))
    }
    fn similar_songs(&self, _artist_id: &str) -> Vec<u32> {
        Vec::new()
    }
    fn artist_info(&self, _artist_id: &str) -> ArtistInfo {
        ArtistInfo { biography: SharedString::default(), similar: Vec::new() }
    }
    /// The artist page's Similar Artists as indexes into `artists`. Empty until the answer lands,
    /// or when there is none.
    fn similar_artists(&self, _artist_id: &str) -> Vec<u32> {
        Vec::new()
    }
    /// "More by <album artist>" under an album, or `None` to hide it.
    fn more_by(&self, _album_id: &str) -> Option<MoreBy> {
        None
    }
    /// Deezer's `rank` for an album's songs (song index to rank), only with the Deezer switch on; empty until the answer lands, and songs Deezer has no match for are left out.
    fn album_heat(&self, _album_id: &str) -> std::collections::HashMap<u32, u64> {
        std::collections::HashMap::new()
    }
    /// an empty answer (offline, or no match) is asked again each time the album opens, not kept for the session
    fn reask_missing_heat(&self, _album_id: &str) {}
    /// empty until the answer lands.
    fn upcoming_for_artist(&self, _artist_id: &str) -> Vec<Upcoming> {
        Vec::new()
    }
    fn upcoming_home(&self) -> Vec<Upcoming> {
        Vec::new()
    }
    /// never asks Deezer.
    fn upcoming_matching(&self, _query: &str) -> Vec<Upcoming> {
        Vec::new()
    }
    fn upcoming_page(&self, _id: u64) -> UpcomingView {
        UpcomingView::Missing
    }
    /// Whether the server has sharing turned on.
    fn can_share(&self) -> bool {
        false
    }
    /// For You. Sample data only has Not Played Recently, from the albums' own dates.
    fn for_you(&self) -> Arc<ForYouPage> {
        let albums = self.albums();
        let now = navia_core::library::taste::now_unix();
        let mut stale: Vec<u32> = (0..albums.len() as u32).filter(|i| { let a = &albums[*i as usize]; a.plays >= 3 && a.last_played > 0 && a.last_played < now - 90 * 86_400 }).collect();
        stale.sort_by_key(|i| albums[*i as usize].last_played);
        stale.truncate(20);
        Arc::new(ForYouPage { not_played_recently: stale, ..Default::default() })
    }
    /// Recent-taste rows. Without dated plays, as in sample data, it uses all-time order, picks the
    /// first played artist with four albums for World of, and skips rows that need dates.
    fn home_taste(&self) -> HomeTaste {
        let (artists, albums) = (self.artists(), self.albums());
        let played_artists: Vec<u32> = (0..artists.len() as u32).filter(|i| artists[*i as usize].plays > 0).collect();
        let world_of = played_artists.iter().copied().find(|i| self.artist_albums(&artists[*i as usize].id).len() >= 4);
        // Artist Radio from the sample's similar artists by name, by the core's rule.
        let by_name: std::collections::HashMap<&str, u32> = artists.iter().enumerate().map(|(i, a)| (a.name.as_ref(), i as u32)).collect();
        let stations = played_artists
            .iter()
            .take(navia_core::stations::CANDIDATES)
            .filter_map(|seed| {
                let mut neighbours: Vec<u32> = Vec::new();
                for name in self.artist_info(&artists[*seed as usize].id).similar {
                    if let Some(i) = by_name.get(name.as_ref()).copied().filter(|i| i != seed && artists[*i as usize].image.is_some() && !neighbours.contains(i)) {
                        neighbours.push(i);
                    }
                }
                (neighbours.len() >= navia_core::stations::MIN_NEIGHBOURS).then_some((*seed, neighbours))
            })
            .take(navia_core::stations::STATIONS_MAX)
            .collect();
        let essentials = played_artists
            .iter()
            .copied()
            .take(navia_core::library::essentials::LOOK_AT)
            .filter(|i| self.artist_top_songs(&artists[*i as usize].id).len() >= navia_core::library::essentials::ESSENTIALS_MIN)
            .take(navia_core::library::essentials::ESSENTIALS)
            .collect();
        HomeTaste { albums: (0..albums.len() as u32).filter(|i| albums[*i as usize].plays > 0).collect(), artists: played_artists, world_of, stations, essentials, ..Default::default() }
    }
}
