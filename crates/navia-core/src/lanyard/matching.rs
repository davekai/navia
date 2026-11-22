//! Finds a friend's Spotify song in the listener's synced library.
//! Strict on purpose, since "Not in your library" is better than playing the wrong song.
//! Titles, artists and recording markers must agree, and the length must be close.

use crate::client::Song;
use crate::library::Library;
use crate::library::text::normalize;

use super::SpotifyTrack;

/// How many same-titled songs are compared at most, most played first.
const CANDIDATES: u32 = 200;

/// Prefixes of a credit decoration, like "(feat. X)" or " - ft. X".
const CREDIT_PREFIXES: &[&str] = &["feat. ", "feat ", "ft. ", "ft ", "featuring ", "with "];
/// Words that make a different recording. Both sides must agree on them.
const RECORDING: &[&str] = &["live", "acoustic", "instrumental", "karaoke", "acapella", "cappella", "unplugged", "demo"];
/// Words that make a remix. Whatever else is in the note names the remixer.
const REMIX: &[&str] = &["remix", "remixed", "rework", "bootleg", "vip", "flip"];
/// Words of an edition note that leaves the recording as it is.
const EDITION: &[&str] = &[
    "remaster", "remastered", "version", "edit", "radio", "single", "album", "mono", "stereo", "deluxe", "explicit", "clean", "bonus",
    "track", "anniversary", "expanded", "edition", "original", "mix", "extended", "digital", "master", "mastered", "the", "a", "of", "from",
    "soundtrack", "remasterizado", "remasterisé", "remasterisee", "remasterizada",
];
/// Words in a "mix" note that don't name a remixer.
const PLAIN_MIX: &[&str] = &["original", "extended", "radio", "club", "album", "single", "mix", "edit", "version", "the"];

/// A title or name for comparing, normalized with "&" as "and", no punctuation and no leading "the".
pub fn key(text: &str) -> String {
    let spaced = text.replace('&', " and ").replace(['’', '\''], "");
    let n = normalize(&spaced);
    let words: Vec<String> = n
        .split(' ')
        .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>())
        .filter(|w| !w.is_empty())
        .collect();
    let words = if words.len() > 1 && words[0] == "the" { &words[1..] } else { &words[..] };
    words.join(" ")
}

/// A title taken apart.
#[derive(Debug, Default, PartialEq, Eq)]
struct Title {
    /// The title without credits and edition notes, as [`key`].
    core: String,
    /// What makes it a different recording, sorted, from [`RECORDING`] words and "remix".
    markers: Vec<&'static str>,
    /// Remixers named, as [`key`].
    remixers: Vec<String>,
}

/// What one decoration is.
enum Note {
    /// Feature credits or an edition note, not part of the title.
    Drop,
    /// A different recording.
    Recording(Vec<&'static str>, Vec<String>),
    /// Part of the title, like "(Part 2)".
    Keep,
}

fn note(inner: &str) -> Note {
    let lower = inner.trim().to_lowercase();
    if CREDIT_PREFIXES.iter().any(|p| lower.starts_with(p)) || lower.starts_with("from ") {
        return Note::Drop;
    }
    let words: Vec<String> = key(inner).split(' ').filter(|w| !w.is_empty()).map(str::to_string).collect();
    if words.is_empty() {
        return Note::Drop;
    }
    let is_year = |w: &str| w.len() == 4 && w.bytes().all(|b| b.is_ascii_digit());
    let mut markers: Vec<&'static str> = RECORDING.iter().copied().filter(|m| words.iter().any(|w| w == m)).collect();
    let remix = words.iter().any(|w| REMIX.contains(&w.as_str()));
    let named_mix = words.iter().any(|w| w == "mix") && words.iter().any(|w| !PLAIN_MIX.contains(&w.as_str()) && !is_year(w));
    let mut remixers = Vec::new();
    if remix || named_mix {
        markers.push("remix");
        let name: Vec<&str> = words.iter().map(String::as_str).filter(|w| !REMIX.contains(w) && !PLAIN_MIX.contains(w) && !is_year(w)).collect();
        if !name.is_empty() {
            remixers.push(name.join(" "));
        }
    }
    if !markers.is_empty() {
        return Note::Recording(markers, remixers);
    }
    // "25th Anniversary Edition".
    let is_ordinal = |w: &str| w.is_ascii() && w.len() > 2 && w[..w.len() - 2].bytes().all(|b| b.is_ascii_digit()) && ["st", "nd", "rd", "th"].contains(&&w[w.len() - 2..]);
    if words.iter().all(|w| EDITION.contains(&w.as_str()) || is_year(w) || is_ordinal(w)) && words.iter().any(|w| !matches!(w.as_str(), "the" | "a" | "of")) {
        return Note::Drop;
    }
    Note::Keep
}

impl Title {
    fn parse(title: &str) -> Self {
        let mut out = Title::default();
        let mut t = title.trim().to_string();
        // Notes come off the end one at a time, either a trailing bracket or a " - " suffix.
        loop {
            let before = t.len();
            let bracket = t.chars().last().filter(|c| matches!(c, ')' | ']')).and_then(|close| t.rfind(if close == ')' { '(' } else { '[' })).filter(|at| *at > 0);
            let dash = || [" - ", " – ", " — "].iter().filter_map(|d| t.rfind(d).map(|at| (at, d.len()))).max().filter(|(at, _)| *at > 0);
            let (at, inner) = match (bracket, dash()) {
                (Some(at), _) => (at, &t[at + 1..t.len() - 1]),
                (None, Some((at, len))) => (at, &t[at + len..]),
                (None, None) => break,
            };
            match note(inner) {
                Note::Keep => break,
                Note::Drop => {}
                Note::Recording(m, r) => {
                    out.markers.extend(m);
                    out.remixers.extend(r);
                }
            }
            t.truncate(at);
            t = t.trim_end().to_string();
            if t.len() == before || t.is_empty() {
                break;
            }
        }
        // An unbracketed "Song feat. X".
        let lower = t.to_lowercase();
        if let Some(at) = [" feat. ", " feat ", " ft. ", " featuring "].iter().filter_map(|p| lower.find(p)).filter(|at| *at > 0).min()
            && t.is_char_boundary(at)
        {
            t.truncate(at);
        }
        out.core = key(&t);
        if out.core.is_empty() {
            out.core = key(title);
        }
        out.markers.sort_unstable();
        out.markers.dedup();
        out.remixers.sort();
        out.remixers.dedup();
        out
    }

    /// Adds "live" when the album is a live album, since live songs are often tagged without the note.
    fn with_album(mut self, album: &str) -> Self {
        if key(album).split(' ').any(|w| w == "live") && !self.markers.contains(&"live") {
            self.markers.push("live");
            self.markers.sort_unstable();
        }
        self
    }
}

/// An album's name for comparing: edition notes and " - Single" / " - EP" off.
pub fn album_key(album: &str) -> String {
    let mut a = album.trim().to_string();
    for suffix in [" - Single", " - EP"] {
        if a.to_lowercase().ends_with(&suffix.to_lowercase()) {
            a.truncate(a.len() - suffix.len());
        }
    }
    Title::parse(&a).core
}

/// a song title for comparing: feature credits and edition notes off, as [`key`].
pub fn title_key(title: &str) -> String {
    Title::parse(title).core
}

/// Splits a credit into names: "A feat. B", "A & B", "A, B", "A; B", Navidrome's "A • B".
pub fn names(credit: &str) -> Vec<String> {
    let mut parts = vec![credit.to_string()];
    for sep in [" • ", " · ", " feat. ", " feat ", " ft. ", " featuring ", " & ", " x ", " + ", ", ", "; ", ";", " / "] {
        parts = parts.into_iter().flat_map(|p| p.to_lowercase().split(sep).map(str::to_string).collect::<Vec<_>>()).collect();
    }
    parts.iter().map(|p| key(p)).filter(|p| !p.is_empty()).collect()
}

/// separators after a credit's first artist, matched in ASCII lowercase.
const CREDIT_SEPARATORS: &[&str] = &[" feat. ", " feat ", " ft. ", " ft ", " featuring ", " with ", " & ", " x ", " • ", " · ", ", ", "; ", " / "];

/// the first credited artist of a credit: "A feat. B", "A ft. B", "A with B", "A & B" and "A x B"
/// give "A". for a retry after the whole credit found nothing.
pub fn first_credited(credit: &str) -> &str {
    let lower = credit.to_ascii_lowercase();
    let cut = CREDIT_SEPARATORS.iter().filter_map(|sep| lower.find(sep)).filter(|at| *at > 0).min().unwrap_or(credit.len());
    credit[..cut].trim()
}

/// a title without any bracketed or dashed suffix and without an unbracketed feature credit:
/// "Song (Remastered)", "Song - 2011 Remaster", "Song (Live)" and "Song [Deluxe]" give "Song". for a
/// retry after the title as given found nothing.
pub fn bare_title(title: &str) -> &str {
    let lower = title.to_ascii_lowercase();
    let cut = ["(", "[", " - ", " – ", " — ", " feat. ", " feat ", " ft. ", " featuring "].iter().filter_map(|m| lower.find(m)).filter(|at| *at > 0).min().unwrap_or(title.len());
    title[..cut].trim()
}

/// A library song's artists, first credited first.
fn song_artists(song: &Song) -> Vec<String> {
    let mut out: Vec<String> = song.artists.iter().map(|a| key(&a.name)).filter(|a| !a.is_empty()).collect();
    for credit in [song.display_artist.as_deref(), song.artist.as_deref()].into_iter().flatten() {
        for n in names(credit) {
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out
}

/// Whether a length difference is small enough to be the same recording, 10 s or 5% of a long song.
pub fn length_allows(theirs_ms: u64, ours_s: u64) -> bool {
    let theirs_s = theirs_ms as f64 / 1000.0;
    (theirs_s - ours_s as f64).abs() <= (theirs_s * 0.05).max(10.0)
}

/// What a friend's song is wanted as.
struct Wanted {
    title: Title,
    full: String,
    artists: Vec<String>,
    album: String,
    /// The album is a Dolby Atmos or other immersive mix ([`immersive`]).
    immersive: bool,
    duration_ms: Option<u64>,
    /// matched on [`bare_title`]s, whatever the decorations say.
    bare: bool,
}

/// Whether an album name says it's an immersive mix like Atmos or surround.
/// Such mixes are kept apart from the stereo one when both are in the library.
fn immersive(album: &str) -> bool {
    let words = key(album);
    words.split(' ').any(|w| matches!(w, "atmos" | "spatial" | "surround" | "51" | "71" | "binaural"))
}

impl Wanted {
    fn new(track: &SpotifyTrack) -> Self {
        let artists: Vec<String> = track.artists().iter().map(|a| key(a)).filter(|a| !a.is_empty()).collect();
        Self::of(&track.song, artists, &track.album, track.duration_ms())
    }

    fn of(title: &str, artists: Vec<String>, album: &str, duration_ms: Option<u64>) -> Self {
        Self { title: Title::parse(title).with_album(album), full: key(title), artists, album: album_key(album), immersive: immersive(album) || immersive(title), duration_ms, bare: false }
    }

    fn bare(title: &str, artist: String, album: &str, duration_ms: Option<u64>) -> Self {
        let core = key(bare_title(title));
        Self { title: Title { core, ..Default::default() }, full: key(title), artists: vec![artist], album: album_key(album), immersive: immersive(album) || immersive(title), duration_ms, bare: true }
    }

    /// How well a library song fits, or `None` when it isn't this recording.
    fn score(&self, song: &Song) -> Option<i64> {
        let album = song.album.as_deref().unwrap_or_default();
        if self.bare {
            if key(bare_title(&song.title)) != self.title.core {
                return None;
            }
        } else {
            let title = Title::parse(&song.title).with_album(album);
            if title.core != self.title.core || title.markers != self.title.markers {
                return None;
            }
            if !self.title.remixers.is_empty() && !title.remixers.is_empty() && self.title.remixers != title.remixers {
                return None;
            }
        }
        let ours = song_artists(song);
        let (Some(their_first), Some(our_first)) = (self.artists.first(), ours.first()) else { return None };
        if !ours.contains(their_first) && !self.artists.contains(our_first) {
            return None;
        }
        let mut score = 0i64;
        if let (Some(d), Some(s)) = (self.duration_ms, song.duration.filter(|s| *s > 0)) {
            if !length_allows(d, s as u64) {
                return None;
            }
            score -= ((d as i64) - s * 1000).abs() / 1000;
        }
        if !self.album.is_empty() && album_key(album) == self.album {
            score += 100;
        }
        if key(&song.title) == self.full {
            score += 20;
        }
        if our_first == their_first {
            score += 10;
        }
        if (immersive(album) || immersive(&song.title)) != self.immersive {
            score -= 50;
        }
        Some(score)
    }
}

/// The library's song for a friend's Spotify song, with its row id, or `None`.
pub fn find(library: &Library, track: &SpotifyTrack) -> Option<(u32, Song)> {
    find_wanted(library, Wanted::new(track))
}

/// The library's song for a song known by name from another service, with the same rules as [`find`].
pub fn find_named(library: &Library, title: &str, artists: &[&str], album: &str, duration_ms: Option<u64>) -> Option<(u32, Song)> {
    let mut keys: Vec<String> = Vec::new();
    for credit in artists {
        for name in names(credit) {
            if !keys.contains(&name) {
                keys.push(name);
            }
        }
    }
    find_wanted(library, Wanted::of(title, keys, album, duration_ms))
}

/// [`find_named`], and when that finds nothing, again with the [`bare_title`] and the
/// [`first_credited`] artist only. the bare names only find the match: the song is still the
/// library's own, whatever its title says (a remaster, a live take).
pub fn find_named_or_bare(library: &Library, title: &str, artists: &[&str], album: &str, duration_ms: Option<u64>) -> Option<(u32, Song)> {
    find_named(library, title, artists, album, duration_ms).or_else(|| {
        let artist = key(first_credited(artists.first()?));
        if artist.is_empty() {
            return None;
        }
        find_wanted(library, Wanted::bare(title, artist, album, duration_ms))
    })
}

fn find_wanted(library: &Library, wanted: Wanted) -> Option<(u32, Song)> {
    if wanted.title.core.is_empty() || wanted.artists.is_empty() {
        return None;
    }
    let candidates = library.songs_titled(&wanted.title.core, CANDIDATES).ok()?;
    best(&wanted, candidates)
}

/// The best candidate, with ties going to the first, most played one.
fn best(wanted: &Wanted, candidates: Vec<(u32, Song)>) -> Option<(u32, Song)> {
    let mut found: Option<(i64, (u32, Song))> = None;
    for (rowid, song) in candidates {
        if let Some(score) = wanted.score(&song)
            && found.as_ref().is_none_or(|(best, _)| score > *best)
        {
            found = Some((score, (rowid, song)));
        }
    }
    found.map(|(_, hit)| hit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ArtistRef;

    fn track(song: &str, artist: &str, album: &str, secs: u64) -> SpotifyTrack {
        SpotifyTrack { song: song.into(), artist: artist.into(), album: album.into(), start_ms: Some(1000), end_ms: Some(1000 + secs * 1000), ..Default::default() }
    }

    fn song(id: &str, title: &str, artist: &str, album: &str, secs: i64) -> (u32, Song) {
        let artists = names(artist).into_iter().map(|n| ArtistRef { id: String::new(), name: n }).take(1).collect();
        (id.len() as u32, Song { id: id.into(), title: title.into(), artist: Some(artist.into()), album: Some(album.into()), duration: Some(secs), artists, ..Default::default() })
    }

    fn pick(t: &SpotifyTrack, songs: Vec<(u32, Song)>) -> Option<String> {
        best(&Wanted::new(t), songs).map(|(_, s)| s.id)
    }

    #[test]
    fn the_stereo_album_wins_over_an_atmos_one_unless_atmos_is_wanted() {
        let songs = || vec![song("atmos", "Song", "Artist", "Album (Dolby Atmos mix)", 200), song("stereo", "Song", "Artist", "Album", 200)];
        assert_eq!(pick(&track("Song", "Artist", "Album", 200), songs()).as_deref(), Some("stereo"));
        assert_eq!(pick(&track("Song", "Artist", "Album (Dolby Atmos)", 200), songs()).as_deref(), Some("atmos"));
    }

    #[test]
    fn titles_lose_credits_and_editions_but_keep_recordings() {
        assert_eq!(Title::parse("Song (feat. Guest)").core, "song");
        assert_eq!(Title::parse("Song - Remastered 2011").core, "song");
        assert_eq!(Title::parse("Song - 2011 Remaster").core, "song");
        assert_eq!(Title::parse("Song [Radio Edit]").core, "song");
        assert_eq!(Title::parse("Song feat. Guest").core, "song");
        assert_eq!(Title::parse("Song - From \"The Movie\"").core, "song");
        assert_eq!(Title::parse("Song (Part 2)").core, "song part 2");
        assert_eq!(Title::parse("Don’t Stop Me Now").core, Title::parse("Dont Stop Me Now").core);
        assert_eq!(Title::parse("Rock & Roll").core, "rock and roll");
        let live = Title::parse("Song - Live at Wembley 1986");
        assert_eq!((live.core.as_str(), live.markers.clone()), ("song", vec!["live"]));
        let remix = Title::parse("Song - Kygo Remix");
        assert_eq!((remix.markers.clone(), remix.remixers.clone()), (vec!["remix"], vec!["kygo".to_string()]));
        assert_eq!(Title::parse("Song (Original Mix)").markers, Vec::<&str>::new());
        assert_eq!(Title::parse("Beyoncé").core, "beyonce");
    }

    #[test]
    fn matches_title_and_artist_then_album_then_length() {
        let t = track("Song (feat. Guest) - Remastered 2011", "Main; Guest", "The Album (Deluxe Edition)", 200);
        let other_album = song("compilation", "Song", "Main feat. Guest", "Greatest Hits", 201);
        let same_album = song("album", "Song (feat. Guest)", "Main", "The Album", 205);
        assert_eq!(pick(&t, vec![other_album.clone(), same_album.clone()]).as_deref(), Some("album"));
        // Without the album, the closer length wins.
        let longer = song("longer", "Song", "Main", "Elsewhere", 208);
        assert_eq!(pick(&t, vec![longer, other_album.clone()]).as_deref(), Some("compilation"));
        // The guest's own credit first still matches.
        assert!(pick(&track("Song", "Guest; Main", "", 200), vec![other_album.clone()]).is_some());
    }

    #[test]
    fn never_takes_another_recording() {
        let t = track("Song", "Main", "The Album", 200);
        // Another artist, a live or remixed take, an extended version.
        assert_eq!(pick(&t, vec![song("a", "Song", "Someone Else", "The Album", 200)]), None);
        assert_eq!(pick(&t, vec![song("b", "Song (Live)", "Main", "The Album", 200)]), None);
        assert_eq!(pick(&t, vec![song("c", "Song", "Main", "Live at the Hall", 200)]), None);
        assert_eq!(pick(&t, vec![song("d", "Song (Kygo Remix)", "Main", "Remixes", 200)]), None);
        assert_eq!(pick(&t, vec![song("e", "Song", "Main", "The Album", 290)]), None);
        assert_eq!(pick(&t, vec![song("f", "Song Two", "Main", "The Album", 200)]), None);
        // When a live one is wanted, the live one wins over the studio one.
        let live = track("Song - Live", "Main", "Tour", 200);
        assert_eq!(pick(&live, vec![song("studio", "Song", "Main", "The Album", 200), song("live", "Song (Live)", "Main", "Tour", 203)]).as_deref(), Some("live"));
        // Only the same remix by the same remixer matches.
        let remix = track("Song - Kygo Remix", "Main; Kygo", "Song (Remixes)", 200);
        assert_eq!(pick(&remix, vec![song("x", "Song (Other Remix)", "Main", "R", 200), song("k", "Song (Kygo Remix)", "Main", "R", 200)]).as_deref(), Some("k"));
    }

    #[test]
    fn bare_names_drop_credits_and_every_suffix() {
        assert_eq!(first_credited("Beyoncé feat. JAY Z"), "Beyoncé");
        assert_eq!(first_credited("Meduza with Goodboys"), "Meduza");
        assert_eq!(first_credited("Shakira & Carlos Santana"), "Shakira");
        assert_eq!(first_credited("Marshmello x Khalid"), "Marshmello");
        assert_eq!(first_credited("Dr. Dre Ft. Snoop Dogg"), "Dr. Dre");
        assert_eq!(first_credited("Oasis"), "Oasis");
        assert_eq!(bare_title("Shakermaker (Remastered)"), "Shakermaker");
        assert_eq!(bare_title("Song - 2011 Remaster"), "Song");
        assert_eq!(bare_title("Song (Live)"), "Song");
        assert_eq!(bare_title("Song [Deluxe]"), "Song");
        assert_eq!(bare_title("Song feat. Guest"), "Song");
        assert_eq!(bare_title("(I Can't Get No) Satisfaction"), "(I Can't Get No) Satisfaction");
        let t = |title: &str, artist: &str| Wanted::bare(title, key(first_credited(artist)), "", Some(200_000));
        // the bare retry finds the library's own song whatever its decorations.
        assert_eq!(best(&t("Song (Live)", "Main with Guest"), vec![song("own", "Song - 2011 Remaster", "Main", "A", 201)]).map(|(_, s)| s.id).as_deref(), Some("own"));
        assert_eq!(best(&t("Song", "Main"), vec![song("x", "Song", "Someone Else", "A", 200)]), None, "still the same artist");
        assert_eq!(best(&t("Song", "Main"), vec![song("y", "Song", "Main", "A", 290)]), None, "still the same length");
    }

    #[test]
    fn lengths_allow_a_little() {
        assert!(length_allows(200_000, 209));
        assert!(!length_allows(200_000, 211));
        assert!(length_allows(600_000, 628));
        assert!(!length_allows(600_000, 632));
    }

    #[test]
    fn finds_through_the_library_index() {
        use crate::client::MusicFolder;
        use crate::library::LibraryConfig;
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(&LibraryConfig { path: dir.path().join("lib.sqlite") }).unwrap();
        lib.replace_libraries(&[MusicFolder { id: "1".into(), name: "L".into() }]).unwrap();
        let cur = lib.begin_sync("1", None, false).unwrap();
        let songs: Vec<Song> = [
            song("s1", "Dreams", "Fleetwood Mac", "Rumours", 257),
            song("s2", "Dreams (Live)", "Fleetwood Mac", "The Dance", 270),
            song("s3", "Sweet Dreams (Are Made of This)", "Eurythmics", "Sweet Dreams", 216),
            song("s4", "Beyoncé Song", "Beyoncé", "B", 180),
        ]
        .into_iter()
        .map(|(_, s)| s)
        .collect();
        lib.apply_songs("1", &cur, songs.len() as u32, &songs).unwrap();
        let hit = |t: SpotifyTrack| find(&lib, &t).map(|(_, s)| s.id);
        assert_eq!(hit(track("Dreams - 2004 Remaster", "Fleetwood Mac", "Rumours (Super Deluxe)", 257)).as_deref(), Some("s1"));
        assert_eq!(hit(track("Dreams - Live", "Fleetwood Mac", "The Dance", 271)).as_deref(), Some("s2"));
        assert_eq!(hit(track("Beyonce Song", "Beyonce", "B", 180)).as_deref(), Some("s4"));
        assert_eq!(hit(track("Dreams", "The Cranberries", "Everybody Else", 271)), None);
        assert_eq!(hit(track("Nowhere", "Nobody", "", 100)), None);
    }
}
