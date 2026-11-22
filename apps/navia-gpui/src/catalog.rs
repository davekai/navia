//! Sample data only: albums, artists and genres derived from the sample's songs.
//! The real app gets these from `navia-core`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::SharedString;

use crate::model::{Album, AlbumTrack, Artist, ArtistInfo, Genre, Song};

/// Per-song facts the UI's `Song` doesn't carry, keyed by song id.
#[derive(Clone, Copy, Default)]
pub struct TrackFacts {
    pub number: Option<u32>,
    pub disc: u32,
    pub hi_res: bool,
}

pub struct RawAlbum {
    pub album: Album,
    pub description: SharedString,
}

pub struct RawArtist {
    pub artist: Artist,
    pub biography: SharedString,
    pub similar: Vec<SharedString>,
}

#[derive(Default)]
pub struct Catalog {
    pub albums: Arc<Vec<Album>>,
    pub artists: Arc<Vec<Artist>>,
    pub genres: Vec<Genre>,
    pub album_tracks: HashMap<SharedString, Vec<AlbumTrack>>,
    pub artist_albums: HashMap<SharedString, Vec<u32>>,
    pub artist_songs: HashMap<SharedString, Vec<u32>>,
    pub descriptions: HashMap<SharedString, SharedString>,
    pub info: HashMap<SharedString, ArtistInfo>,
    pub artist_by_name: HashMap<SharedString, u32>,
}

impl Catalog {
    pub fn build(songs: &[Song], facts: &HashMap<SharedString, TrackFacts>, raw_albums: Vec<RawAlbum>, raw_artists: Vec<RawArtist>) -> Self {
        let mut album_tracks: HashMap<SharedString, Vec<AlbumTrack>> = HashMap::new();
        let mut album_plays: HashMap<SharedString, u32> = HashMap::new();
        let mut artist_plays: HashMap<SharedString, u32> = HashMap::new();
        let mut artist_songs_by_name: HashMap<SharedString, Vec<u32>> = HashMap::new();
        let mut genre_songs: HashMap<SharedString, u32> = HashMap::new();
        for (ix, song) in songs.iter().enumerate() {
            let f = facts.get(&song.id).copied().unwrap_or_default();
            album_tracks.entry(song.album_id.clone()).or_default().push(AlbumTrack { song: ix as u32, number: f.number, disc: f.disc, hi_res: f.hi_res });
            *album_plays.entry(song.album_id.clone()).or_default() += song.plays;
            for name in &song.artists {
                *artist_plays.entry(name.clone()).or_default() += song.plays;
                artist_songs_by_name.entry(name.clone()).or_default().push(ix as u32);
            }
            if !song.genre.is_empty() {
                *genre_songs.entry(song.genre.clone()).or_default() += 1;
            }
        }
        for tracks in album_tracks.values_mut() {
            tracks.sort_by_key(|t| (t.disc, t.number.unwrap_or(u32::MAX), t.song));
        }

        let mut descriptions = HashMap::new();
        let mut albums: Vec<Album> = Vec::with_capacity(raw_albums.len());
        for raw in raw_albums {
            let mut album = raw.album;
            album.plays = album_plays.get(&album.id).copied().unwrap_or(0);
            if !raw.description.is_empty() {
                descriptions.insert(album.id.clone(), raw.description);
            }
            albums.push(album);
        }
        albums.sort_by(|a, b| b.plays.cmp(&a.plays).then_with(|| a.name.as_ref().cmp(b.name.as_ref())));

        let mut info = HashMap::new();
        let mut artists: Vec<Artist> = Vec::with_capacity(raw_artists.len());
        for raw in raw_artists {
            let mut artist = raw.artist;
            artist.plays = artist_plays.get(&artist.name).copied().unwrap_or(0);
            info.insert(artist.id.clone(), ArtistInfo { biography: raw.biography, similar: raw.similar });
            artists.push(artist);
        }
        artists.sort_by(|a, b| b.plays.cmp(&a.plays).then_with(|| a.name.as_ref().cmp(b.name.as_ref())));
        artists.retain(|a| artist_songs_by_name.contains_key(&a.name));

        let artist_by_name: HashMap<SharedString, u32> = artists.iter().enumerate().map(|(i, a)| (a.name.clone(), i as u32)).collect();

        let mut artist_albums: HashMap<SharedString, Vec<u32>> = HashMap::new();
        let mut seen: HashSet<(usize, SharedString)> = HashSet::new();
        for (ix, album) in albums.iter().enumerate() {
            if let Some(artist) = artist_by_name.get(&album.artist).map(|i| &artists[*i as usize]) {
                if seen.insert((ix, artist.id.clone())) {
                    artist_albums.entry(artist.id.clone()).or_default().push(ix as u32);
                }
            }
        }
        for list in artist_albums.values_mut() {
            list.sort_by_key(|i| std::cmp::Reverse(albums[*i as usize].year));
        }

        let artist_songs: HashMap<SharedString, Vec<u32>> = artists
            .iter()
            .filter_map(|a| artist_songs_by_name.get(&a.name).map(|s| (a.id.clone(), s.clone())))
            .collect();

        let mut genres: Vec<Genre> = genre_songs.into_iter().map(|(name, count)| Genre { name, song_count: count }).collect();
        genres.sort_by(|a, b| b.song_count.cmp(&a.song_count).then_with(|| a.name.as_ref().cmp(b.name.as_ref())));

        Self { albums: Arc::new(albums), artists: Arc::new(artists), genres, album_tracks, artist_albums, artist_songs, descriptions, info, artist_by_name }
    }
}

/// Albums and artists straight from songs, for the generated test library.
pub fn derive_raw(songs: &[Song]) -> (Vec<RawAlbum>, Vec<RawArtist>) {
    let mut albums: HashMap<SharedString, Album> = HashMap::new();
    let mut artists: HashMap<SharedString, Artist> = HashMap::new();
    for song in songs {
        let album = albums.entry(song.album_id.clone()).or_insert_with(|| Album {
            id: song.album_id.clone(),
            name: song.album.clone(),
            artist: song.artist.clone(),
            artist_id: Some(song.artist.clone()),
            year: song.year,
            song_count: 0,
            duration_s: 0,
            genre: song.genre.clone(),
            added: 0,
            plays: 0,
            last_played: 0,
            starred: false,
            explicit: false,
            disc_titles: Vec::new(),
            edition: None,
            edition_on_card: false,
            release_day: None,
        });
        album.song_count += 1;
        album.duration_s += song.duration_s;
        for name in &song.artists {
            artists.entry(name.clone()).or_insert_with(|| Artist { id: name.clone(), name: name.clone(), image: None, plays: 0, starred: false });
        }
    }
    let raw_albums = albums.into_values().map(|album| RawAlbum { album, description: SharedString::default() }).collect();
    let raw_artists = artists.into_values().map(|artist| RawArtist { artist, biography: SharedString::default(), similar: Vec::new() }).collect();
    (raw_albums, raw_artists)
}
