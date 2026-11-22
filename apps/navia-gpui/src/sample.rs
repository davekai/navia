//! Sample data for development (`NAVIA_SAMPLE=1`). Reads a local Navidrome database read-only
//! from `NAVIA_SAMPLE_DB`, or generates data without one.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::SharedString;
use lofty::file::TaggedFileExt;
use rusqlite::{Connection, OpenFlags};

use crate::catalog::{self, Catalog, RawAlbum, RawArtist, TrackFacts};
use crate::model::{Album, AlbumTrack, Artist, ArtistInfo, Genre, Library, LyricLine, PlaylistDetails, PlaylistSummary, RadioStation, Song};

/// A library with nothing in it, for when no server is configured.
pub fn empty() -> Arc<dyn Library> {
    Arc::new(EmptyLibrary)
}

/// The library with no server. It invents nothing, so it has no songs, playlists or stations.
struct EmptyLibrary;

impl Library for EmptyLibrary {
    fn songs(&self) -> Arc<Vec<Song>> {
        Arc::default()
    }
    fn now_playing(&self) -> Option<usize> {
        None
    }
    fn playlists(&self) -> Vec<PlaylistSummary> {
        Vec::new()
    }
    fn album_artwork(&self, _album_id: &str) -> Option<Vec<u8>> {
        None
    }
    fn lyrics(&self, _song_id: &str) -> Vec<LyricLine> {
        Vec::new()
    }
}

pub fn open() -> Arc<dyn Library> {
    if std::env::var_os("NAVIA_FAKE_DATA").is_none() {
        match SqliteLibrary::open() {
            Ok(lib) => return Arc::new(lib),
            Err(e) => eprintln!("navia: read-only database unavailable ({e}), using generated data"),
        }
    }
    Arc::new(FakeLibrary::new(33_675))
}

struct SqliteLibrary {
    songs: Arc<Vec<Song>>,
    now: Option<usize>,
    playlists: Vec<PlaylistSummary>,
    /// Album id to the host path of a file with embedded art.
    art_path: HashMap<SharedString, PathBuf>,
    /// Artist photo id (`ar-<artist id>`) to the server's URL for it.
    artist_art: HashMap<SharedString, String>,
    catalog: Catalog,
    playlist_details: Arc<Vec<PlaylistDetails>>,
    stations: Arc<Vec<RadioStation>>,
    /// Song id to index, for playlist queries.
    song_index: HashMap<SharedString, u32>,
}

/// The local Navidrome database to open read-only, from `NAVIA_SAMPLE_DB`.
fn sample_db() -> Option<String> {
    let path = std::env::var("NAVIA_SAMPLE_DB").ok().filter(|p| !p.is_empty())?;
    Some(format!("file:{path}?mode=ro"))
}

/// Server library folders mapped to local paths, for embedded art.
/// Format is `NAVIA_SAMPLE_PATHS=/music=/srv/music;/other=/mnt/other`.
fn host_roots() -> Vec<(String, String)> {
    let map = std::env::var("NAVIA_SAMPLE_PATHS").unwrap_or_default();
    map.split(';').filter_map(|pair| pair.split_once('=')).map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

impl SqliteLibrary {
    fn open() -> Result<Self, String> {
        let db = sample_db().ok_or("NAVIA_SAMPLE_DB isn't set")?;
        let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI)
            .map_err(|e| e.to_string())?;

        // Play counts are summed across users. No user table.
        let mut plays: HashMap<String, (u32, bool)> = HashMap::new();
        let mut q = c
            .prepare("select item_id, coalesce(sum(play_count),0), coalesce(max(starred),0) from annotation where item_type='media_file' group by 1")
            .map_err(|e| e.to_string())?;
        for row in q
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))
            .map_err(|e| e.to_string())?
            .flatten()
        {
            plays.insert(row.0, (row.1 as u32, row.2 != 0));
        }

        let mut lib_roots: HashMap<i64, String> = HashMap::new();
        let mut q = c.prepare("select id, path from library").map_err(|e| e.to_string())?;
        for row in q.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).map_err(|e| e.to_string())?.flatten() {
            let mut root = row.1.clone();
            for (a, b) in host_roots() {
                if root.starts_with(&a) {
                    root = format!("{b}{}", &root[a.len()..]);
                    break;
                }
            }
            lib_roots.insert(row.0, root);
        }

        let mut art_path = HashMap::new();
        let mut album_genre: HashMap<String, String> = HashMap::new();
        // Album play history comes from its songs' annotations, never the user table.
        let mut album_last: HashMap<String, i64> = HashMap::new();
        let mut q = c
            .prepare(
                "select m.album_id, cast(strftime('%s', max(n.play_date)) as integer) from annotation n join media_file m on m.id=n.item_id and n.item_type='media_file' group by 1",
            )
            .map_err(|e| e.to_string())?;
        for row in q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))).map_err(|e| e.to_string())?.flatten() {
            album_last.insert(row.0, row.1.unwrap_or(0));
        }
        let mut starred_items: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
        let mut q = c.prepare("select item_type, item_id from annotation where starred=1 and item_type in ('album','artist')").map_err(|e| e.to_string())?;
        for row in q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|e| e.to_string())?.flatten() {
            starred_items.entry(row.0).or_default().insert(row.1);
        }
        let album_starred = starred_items.remove("album").unwrap_or_default();
        let artist_starred = starred_items.remove("artist").unwrap_or_default();
        let mut q = c
            .prepare("select id, embed_art_path, library_id, tags from album where missing=0")
            .map_err(|e| e.to_string())?;
        let rows = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows.flatten() {
            let (id, embed, lib, tags) = row;
            if !embed.is_empty() {
                let root = lib_roots.get(&lib).cloned().unwrap_or_default();
                art_path.insert(SharedString::from(id.clone()), PathBuf::from(root).join(embed));
            }
            album_genre.insert(id, genre_of(tags.as_deref().unwrap_or("")));
        }

        let mut q = c
            .prepare(
                "select id,title,artist,album,album_id,year,duration,tags,participants,lyrics,has_cover_art,track_number,disc_number,bit_depth,sample_rate from media_file where missing=0",
            )
            .map_err(|e| e.to_string())?;
        let mut songs: Vec<Song> = Vec::with_capacity(33_700);
        let mut facts: HashMap<SharedString, TrackFacts> = HashMap::with_capacity(33_700);
        let mut rows = q.query([]).map_err(|e| e.to_string())?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let id: String = r.get(0).unwrap_or_default();
            let title: String = r.get(1).unwrap_or_default();
            let artist: String = r.get(2).unwrap_or_default();
            let album: String = r.get(3).unwrap_or_default();
            let album_id: String = r.get(4).unwrap_or_default();
            let year: i64 = r.get(5).unwrap_or_default();
            let duration: f64 = r.get(6).unwrap_or_default();
            let tags: Option<String> = r.get(7).ok().flatten();
            let participants: Option<String> = r.get(8).ok().flatten();
            let lyrics: Option<String> = r.get(9).ok().flatten();
            let number: i64 = r.get(11).unwrap_or_default();
            let disc: i64 = r.get(12).unwrap_or_default();
            let bit_depth: i64 = r.get(13).unwrap_or_default();
            let sample_rate: i64 = r.get(14).unwrap_or_default();
            facts.insert(
                SharedString::from(id.clone()),
                TrackFacts { number: (number > 0).then_some(number as u32), disc: disc.max(0) as u32, hi_res: bit_depth > 16 || sample_rate > 48_000 },
            );
            let (p, starred) = plays.get(&id).copied().unwrap_or((0, false));
            let genre = {
                let g = genre_of(tags.as_deref().unwrap_or(""));
                if g.is_empty() { album_genre.get(&album_id).cloned().unwrap_or_default() } else { g }
            };
            let downloaded = md5_downloaded(&id);
            let artists = artists_of(participants.as_deref().unwrap_or(""), &artist);
            songs.push(Song {
                id: id.into(),
                title: title.into(),
                artist: artist.into(),
                album: album.into(),
                album_id: album_id.into(),
                genre: genre.into(),
                year: year.max(0) as u32,
                duration_s: duration.round().max(0.0) as u32,
                plays: p,
                starred,
                downloaded,
                has_lyrics: lyrics.as_deref().map(|s| s != "[]" && !s.is_empty()).unwrap_or(false),
                artists,
                artist_art: Vec::new(),
                explicit: false,
                added: 0,
                disc: 0,
            });
        }
        songs.sort_by(|a, b| b.plays.cmp(&a.plays).then_with(|| a.title.as_ref().cmp(b.title.as_ref())));

        // Public playlists only, so other users' private lists stay out.
        let mut playlists = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut q = c
            .prepare("select id, name, rules from playlist where public=1 and song_count>=8 order by song_count desc")
            .map_err(|e| e.to_string())?;
        let mut smart = Vec::new();
        for row in q
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?)))
            .map_err(|e| e.to_string())?
            .flatten()
        {
            if !seen.insert(row.1.clone()) {
                continue;
            }
            let is_smart = row.2.as_ref().is_some_and(|r| !r.is_empty());
            let summary = PlaylistSummary { id: row.0.into(), name: row.1.into(), owner: SharedString::default(), readonly: is_smart, smart: is_smart, public: true, mine: std::env::var_os("NAVIA_SAMPLE_NOT_OWNER").is_none() };
            if is_smart {
                smart.push(summary);
            } else if playlists.len() < 13 {
                playlists.push(summary);
            }
        }
        smart.truncate(5);
        playlists.extend(smart);

        let mut raw_albums = Vec::new();
        let mut q = c
            .prepare(
                "select id,name,album_artist,album_artist_id,max_year,song_count,duration,description,cast(strftime('%s', created_at) as integer) from album where missing=0",
            )
            .map_err(|e| e.to_string())?;
        for row in q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, f64>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .flatten()
        {
            let (id, name, artist, artist_id, year, song_count, duration, description, added) = row;
            let genre = album_genre.get(&id).cloned().unwrap_or_default();
            let last_played = album_last.get(&id).copied().unwrap_or(0);
            let starred = album_starred.contains(&id);
            raw_albums.push(RawAlbum {
                album: Album {
                    id: id.into(),
                    name: name.into(),
                    artist: artist.into(),
                    artist_id: (!artist_id.is_empty()).then(|| artist_id.into()),
                    year: year.max(0) as u32,
                    song_count: song_count.max(0) as u32,
                    duration_s: duration.round().max(0.0) as u32,
                    genre: genre.into(),
                    added: added.unwrap_or(0),
                    plays: 0,
                    last_played,
                    starred,
                    explicit: false,
                    disc_titles: Vec::new(),
                    edition: None,
                    edition_on_card: false,
                    release_day: None,
                },
                description: strip_html(&description).into(),
            });
        }
        let mut raw_artists = Vec::new();
        let mut artist_art = HashMap::new();
        let mut q = c.prepare("select id,name,large_image_url,biography,similar_artists from artist where missing=0").map_err(|e| e.to_string())?;
        for row in q
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?)))
            .map_err(|e| e.to_string())?
            .flatten()
        {
            let (id, name, image_url, biography, similar) = row;
            let image = (!image_url.is_empty()).then(|| {
                let key = SharedString::from(format!("ar-{id}"));
                artist_art.insert(key.clone(), image_url.clone());
                key
            });
            let similar: Vec<SharedString> = serde_json::from_str::<serde_json::Value>(&similar)
                .ok()
                .and_then(|v| v.as_array().map(|list| list.iter().filter_map(|a| a.get("name").and_then(|n| n.as_str()).map(|s| SharedString::from(s.to_string()))).collect()))
                .unwrap_or_default();
            let starred = artist_starred.contains(&id);
            raw_artists.push(RawArtist { artist: Artist { id: id.into(), name: name.into(), image, plays: 0, starred }, biography: strip_html(&biography).into(), similar });
        }
        let catalog = Catalog::build(&songs, &facts, raw_albums, raw_artists);
        let photo_of = |name: &SharedString| catalog.artist_by_name.get(name).and_then(|i| catalog.artists[*i as usize].image.clone()).unwrap_or_default();
        for song in &mut songs {
            song.artist_art = song.artists.iter().map(&photo_of).collect();
        }

        let mut details = Vec::new();
        for summary in &playlists {
            let row = c
                .query_row(
                    "select comment,duration,song_count,rules from playlist where id=?1",
                    [summary.id.as_ref()],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?, r.get::<_, i64>(2)?, r.get::<_, Option<String>>(3)?)),
                )
                .ok();
            if let Some((description, duration, song_count, rules)) = row {
                let rules = rules.filter(|r| !r.trim().is_empty());
                details.push(PlaylistDetails {
                    id: summary.id.clone(),
                    name: summary.name.clone(),
                    song_count: song_count.max(0) as u32,
                    duration_s: duration.round().max(0.0) as u32,
                    description: strip_html(&description).into(),
                    smart: rules.is_some(),
                    // The sample only reads public playlists, and has no idea who "you" are.
                    public: true,
                    owned: std::env::var_os("NAVIA_SAMPLE_NOT_OWNER").is_none(),
                    mine: std::env::var_os("NAVIA_SAMPLE_NOT_OWNER").is_none(),
                    owner: if std::env::var_os("NAVIA_SAMPLE_NOT_OWNER").is_some() { "someone else".into() } else { SharedString::default() },
                    rules,
                    readonly: false,
                });
            }
        }
        let mut stations = Vec::new();
        let mut q = c.prepare("select id,name,stream_url from radio order by name").map_err(|e| e.to_string())?;
        for row in q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))).map_err(|e| e.to_string())?.flatten() {
            stations.push(RadioStation { id: row.0.into(), name: row.1.into(), url: row.2.into() });
        }

        // The now-playing pick: a song with lyrics, long enough to show them.
        let now = songs
            .iter()
            .position(|s| s.has_lyrics && s.duration_s > 150)
            .or(if songs.is_empty() { None } else { Some(0) });
        let song_index = songs.iter().enumerate().map(|(i, s)| (s.id.clone(), i as u32)).collect();
        Ok(Self { songs: Arc::new(songs), now, playlists, art_path, artist_art, catalog, playlist_details: Arc::new(details), stations: Arc::new(stations), song_index })
    }
}

fn md5_downloaded(id: &str) -> bool {
    let d = md5::compute(id.as_bytes());
    // A song counts as downloaded when md5(id) mod 5 == 0, a stable pseudo-random pick.
    let mut acc: u128 = 0;
    for b in d.0 {
        acc = (acc << 8) | b as u128;
    }
    acc % 5 == 0
}

const GENRE_RENAMES: [(&str, &str); 3] = [("Hip-Hop", "Hip Hop"), ("Edm", "EDM"), ("R&b", "R&B")];

fn genre_of(tags: &str) -> String {
    if !tags.contains("\"genre\"") {
        return String::new();
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(tags) else {
        return String::new();
    };
    let name = v.get("genre").and_then(|g| g.get(0)).and_then(|g| g.get("value")).and_then(|n| n.as_str()).unwrap_or("").trim();
    for (from, to) in GENRE_RENAMES {
        if name == from {
            return to.to_string();
        }
    }
    name.to_string()
}

fn artists_of(participants: &str, fallback: &str) -> Vec<SharedString> {
    let mut out: Vec<SharedString> = Vec::new();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(participants) {
        if let Some(list) = v.get("artist").and_then(|a| a.as_array()) {
            for a in list {
                if let Some(name) = a.get("name").and_then(|n| n.as_str()) {
                    out.push(SharedString::from(name.to_string()));
                }
            }
        }
    }
    if out.is_empty() {
        out.push(SharedString::from(fallback.to_string()));
    }
    out
}

impl Library for SqliteLibrary {
    fn songs(&self) -> Arc<Vec<Song>> {
        self.songs.clone()
    }
    fn now_playing(&self) -> Option<usize> {
        // Test switch for a server with no play history.
        if std::env::var_os("NAVIA_NO_HISTORY").is_some() { None } else { self.now }
    }
    fn playlists(&self) -> Vec<PlaylistSummary> {
        self.playlists.clone()
    }
    fn album_artwork(&self, album_id: &str) -> Option<Vec<u8>> {
        if album_id.starts_with("ar-") {
            return self.artist_art.get(&SharedString::from(album_id.to_string())).and_then(|url| fetch_public_image(album_id, url));
        }
        let path = self.art_path.get(&SharedString::from(album_id.to_string()))?;
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let ext = ext.to_ascii_lowercase();
            if ["jpg", "jpeg", "png", "webp"].contains(&ext.as_str()) {
                return std::fs::read(path).ok();
            }
        }
        let tagged = lofty::read_from_path(path).ok()?;
        let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
        let pic = tag.pictures().first()?;
        Some(pic.data().to_vec())
    }
    fn lyrics(&self, song_id: &str) -> Vec<LyricLine> {
        read_lyrics(song_id).unwrap_or_default()
    }
    fn lyrics_writers(&self, song_id: &str) -> Vec<String> {
        read_writers(song_id).unwrap_or_default()
    }
    fn albums(&self) -> Arc<Vec<Album>> {
        self.catalog.albums.clone()
    }
    fn artists(&self) -> Arc<Vec<Artist>> {
        self.catalog.artists.clone()
    }
    fn playlist_details(&self) -> Arc<Vec<PlaylistDetails>> {
        self.playlist_details.clone()
    }
    fn genres(&self) -> Arc<Vec<Genre>> {
        Arc::new(self.catalog.genres.clone())
    }
    fn radio_stations(&self) -> Arc<Vec<RadioStation>> {
        self.stations.clone()
    }
    fn album_tracks(&self, album_id: &str) -> Vec<AlbumTrack> {
        self.catalog.album_tracks.get(album_id).cloned().unwrap_or_default()
    }
    fn album_description(&self, album_id: &str) -> SharedString {
        self.catalog.descriptions.get(album_id).cloned().unwrap_or_default()
    }
    fn playlist_songs(&self, playlist_id: &str) -> Vec<u32> {
        read_playlist(playlist_id, &self.song_index).unwrap_or_default()
    }
    fn artist_albums(&self, artist_id: &str) -> Vec<u32> {
        self.catalog.artist_albums.get(artist_id).cloned().unwrap_or_default()
    }
    fn artist_top_songs(&self, artist_id: &str) -> Vec<u32> {
        top_songs(&self.songs, self.catalog.artist_songs.get(artist_id).map_or(&[], Vec::as_slice))
    }
    fn artist_songs(&self, artist_id: &str) -> Vec<u32> {
        self.catalog.artist_songs.get(artist_id).cloned().unwrap_or_default()
    }
    fn similar_songs(&self, artist_id: &str) -> Vec<u32> {
        similar_songs(&self.catalog, &self.songs, artist_id)
    }
    fn artist_info(&self, artist_id: &str) -> ArtistInfo {
        self.catalog.info.get(artist_id).cloned().unwrap_or(ArtistInfo { biography: SharedString::default(), similar: Vec::new() })
    }
    fn can_share(&self) -> bool {
        true
    }
}

/// the song's lyricists, else its composers, from the database's participants.
fn read_writers(song_id: &str) -> Option<Vec<String>> {
    let home = std::env::var("HOME").ok()?;
    let db = format!("file:{home}/.containers/navidrome/data/navidrome.db?mode=ro");
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI).ok()?;
    let text: String = c.query_row("select participants from media_file where id=?1", [song_id], |r| r.get(0)).ok()?;
    let roles: serde_json::Value = serde_json::from_str(&text).ok()?;
    let names = |role: &str| roles[role].as_array().map(|a| a.iter().filter_map(|p| p["name"].as_str().map(str::to_string)).collect::<Vec<_>>()).unwrap_or_default();
    let lyricists = names("lyricist");
    Some(if lyricists.is_empty() { names("composer") } else { lyricists })
}

fn read_lyrics(song_id: &str) -> Option<Vec<LyricLine>> {
    let db = sample_db()?;
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI).ok()?;
    let text: String = c.query_row("select lyrics from media_file where id=?1", [song_id], |r| r.get(0)).ok()?;
    // The same choice and tidying as the server's answer, through the core.
    let lists: Vec<navia_core::client::StructuredLyrics> = serde_json::from_str(&text).ok()?;
    let found = navia_core::lyrics::from_structured(lists)?;
    let synced = found.synced;
    Some(found.lines.into_iter().map(|l| LyricLine { start_ms: l.start_ms.unwrap_or(0), text: SharedString::from(l.text.trim().to_string()), synced, timed: None, backing: None, backing_first: false }).collect())
}

/// Deterministic generated library, for machines without a Navidrome database.
struct FakeLibrary {
    songs: Arc<Vec<Song>>,
    catalog: Catalog,
}

impl FakeLibrary {
    fn new(count: usize) -> Self {
        const WORDS: [&str; 16] = ["Northern", "Paper", "Slow", "Glass", "River", "Static", "Morning", "Velvet", "Hollow", "Golden", "Distant", "Quiet", "Electric", "Lonely", "Silver", "Winter"];
        const NOUNS: [&str; 12] = ["Lights", "Birds", "Signal", "Harbor", "Ghosts", "Season", "Echoes", "Rooms", "Fever", "Tide", "Promise", "Roads"];
        const GENRES: [&str; 7] = ["Electronic", "Pop", "Indie Rock", "Hip Hop", "Jazz", "R&B", "Ambient"];
        let mut songs = Vec::with_capacity(count);
        let mut rng = 0x1357_9bdfu32;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            rng
        };
        for i in 0..count {
            let (a, b, c) = (next() as usize, next() as usize, next() as usize);
            let artist = format!("{} {}", WORDS[a % WORDS.len()], NOUNS[b % NOUNS.len()]);
            let album = format!("{} {}", WORDS[b % WORDS.len()], NOUNS[c % NOUNS.len()]);
            let album_id = format!("fake-album-{}", c % 900);
            songs.push(Song {
                id: format!("fake-{i}").into(),
                title: format!("{} {}", WORDS[c % WORDS.len()], NOUNS[a % NOUNS.len()]).into(),
                artist: artist.clone().into(),
                album: album.into(),
                album_id: album_id.into(),
                genre: GENRES[(a ^ b) % GENRES.len()].into(),
                year: 1980 + (a % 45) as u32,
                duration_s: 120 + (b % 300) as u32,
                plays: (count - i) as u32,
                starred: a % 7 == 0,
                downloaded: b % 5 == 0,
                has_lyrics: c % 2 == 0,
                artists: vec![artist.into()],
                    artist_art: Vec::new(),
                explicit: false,
                added: 0,
                disc: 0,
            });
        }
        let (raw_albums, raw_artists) = catalog::derive_raw(&songs);
        let catalog = Catalog::build(&songs, &HashMap::new(), raw_albums, raw_artists);
        Self { songs: Arc::new(songs), catalog }
    }
}

impl Library for FakeLibrary {
    fn songs(&self) -> Arc<Vec<Song>> {
        self.songs.clone()
    }
    fn now_playing(&self) -> Option<usize> {
        Some(0)
    }
    fn playlists(&self) -> Vec<PlaylistSummary> {
        (0..14).map(|i| PlaylistSummary { id: format!("p{i}").into(), name: format!("Sample Mix {}", i + 1).into(), owner: SharedString::default(), readonly: false, smart: false, public: true, mine: true }).collect()
    }
    fn album_artwork(&self, _album_id: &str) -> Option<Vec<u8>> {
        None
    }
    fn lyrics(&self, _song_id: &str) -> Vec<LyricLine> {
        Vec::new()
    }
    fn albums(&self) -> Arc<Vec<Album>> {
        self.catalog.albums.clone()
    }
    fn artists(&self) -> Arc<Vec<Artist>> {
        self.catalog.artists.clone()
    }
    fn genres(&self) -> Arc<Vec<Genre>> {
        Arc::new(self.catalog.genres.clone())
    }
    fn album_tracks(&self, album_id: &str) -> Vec<AlbumTrack> {
        self.catalog.album_tracks.get(album_id).cloned().unwrap_or_default()
    }
    fn artist_albums(&self, artist_id: &str) -> Vec<u32> {
        self.catalog.artist_albums.get(artist_id).cloned().unwrap_or_default()
    }
    fn artist_top_songs(&self, artist_id: &str) -> Vec<u32> {
        top_songs(&self.songs, self.catalog.artist_songs.get(artist_id).map_or(&[], Vec::as_slice))
    }
    fn artist_songs(&self, artist_id: &str) -> Vec<u32> {
        self.catalog.artist_songs.get(artist_id).cloned().unwrap_or_default()
    }
}

/// The artist's most played songs with repeats of the same title removed.
fn top_songs(songs: &[Song], artist_songs: &[u32]) -> Vec<u32> {
    let mut seen = std::collections::HashSet::new();
    let mut top = Vec::new();
    for &ix in artist_songs {
        let song = &songs[ix as usize];
        if song.plays == 0 && top.len() >= 5 {
            break;
        }
        if seen.insert(song.title.to_lowercase()) {
            top.push(ix);
            if top.len() == 20 {
                break;
            }
        }
    }
    top
}

/// Sample songs by artists the server calls similar. Empty without similar-artist data.
fn similar_songs(catalog: &Catalog, songs: &[Song], artist_id: &str) -> Vec<u32> {
    let Some(info) = catalog.info.get(artist_id) else { return Vec::new() };
    let mut found: Vec<u32> = info
        .similar
        .iter()
        .filter_map(|name| catalog.artist_by_name.get(name))
        .filter_map(|i| catalog.artist_songs.get(&catalog.artists[*i as usize].id))
        .flat_map(|list| list.iter().copied().take(8))
        .collect();
    found.sort_by_key(|ix| songs[*ix as usize].title.len() ^ artist_id.len());
    found.truncate(40);
    if found.len() < 5 { Vec::new() } else { found }
}

/// Server text can hold simple HTML.
fn strip_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'").trim().to_string()
}

/// Sample-only: artist photos come from the public URLs the server already found, cached on disk.
fn fetch_public_image(key: &str, url: &str) -> Option<Vec<u8>> {
    let dir = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?.join("navia-sample/artists");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}.img", md5::compute(key.as_bytes()).0.iter().map(|b| format!("{b:02x}")).collect::<String>()));
    if let Ok(bytes) = std::fs::read(&path) {
        return (!bytes.is_empty()).then_some(bytes);
    }
    let output = std::process::Command::new("curl").args(["-fsL", "--max-time", "12", url]).output().ok()?;
    let bytes = if output.status.success() { output.stdout } else { Vec::new() };
    std::fs::write(&path, &bytes).ok()?;
    (!bytes.is_empty()).then_some(bytes)
}

fn read_playlist(playlist_id: &str, index: &HashMap<SharedString, u32>) -> Option<Vec<u32>> {
    let db = sample_db()?;
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI).ok()?;
    let mut q = c.prepare("select media_file_id from playlist_tracks where playlist_id=?1 order by id").ok()?;
    let rows = q.query_map([playlist_id], |r| r.get::<_, String>(0)).ok()?;
    Some(rows.flatten().filter_map(|id| index.get(&SharedString::from(id)).copied()).collect())
}
