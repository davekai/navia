//! Lyrics cached on disk as small JSON files, so they show at once on replay and offline.
//! Songs without lyrics are remembered for a shorter time, so they aren't asked about on every play.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::capabilities::ServerFeatures;
use crate::client::{Client, CueLine, StructuredLyrics};
use crate::error::Error;

/// Plenty for tens of thousands of songs' lyrics.
pub const DEFAULT_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Lyrics are used as they are for this long before the server is asked again.
const FRESH_FOR: Duration = Duration::from_secs(7 * 24 * 3600);
/// A song with no lyrics is asked about again after this long.
const NONE_FRESH_FOR: Duration = Duration::from_secs(24 * 3600);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricLine {
    /// When the line starts, for synced lyrics.
    pub start_ms: Option<u32>,
    pub text: String,
    /// The exact end, when the server's source has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<u32>,
    /// Real word or syllable timing from `songLyrics` v2, in order and covering parts of `text`.
    /// Empty when the line has none, and the apps then estimate the fill.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<LyricWord>,
}

/// One timed word or syllable of a line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricWord {
    /// Byte range in the line's `text`, end exclusive, always on character boundaries.
    pub start: u32,
    pub end: u32,
    pub start_ms: u32,
    /// When the word ends. `None` when the source only has starts, and the next start or line end serves.
    pub end_ms: Option<u32>,
}

impl LyricLine {
    pub fn plain(start_ms: Option<u32>, text: impl Into<String>) -> Self {
        Self { start_ms, text: text.into(), end_ms: None, words: Vec::new() }
    }

    /// How much of the line is sung at `ms`, as a fraction of its characters from 0 to 1.
    /// `None` without word timing. Inside a word the fill moves evenly, and between words it waits.
    pub fn sung_fraction(&self, ms: u32) -> Option<f32> {
        let first = self.words.first()?;
        let total = self.text.chars().count().max(1) as f32;
        let chars_to = |byte: u32| self.text.get(..byte as usize).map_or(0, |s| s.chars().count()) as f32;
        if ms < first.start_ms {
            return Some(chars_to(first.start) / total);
        }
        for (i, w) in self.words.iter().enumerate() {
            if ms < w.start_ms {
                return Some(chars_to(w.start) / total);
            }
            let end_ms = w.end_ms.or_else(|| self.words.get(i + 1).map(|n| n.start_ms)).or(self.end_ms).unwrap_or(w.start_ms);
            if ms < end_ms {
                let t = (ms - w.start_ms) as f32 / (end_ms - w.start_ms).max(1) as f32;
                let (a, b) = (chars_to(w.start), chars_to(w.end));
                return Some((a + t * (b - a)) / total);
            }
        }
        // Anything after the last word, such as punctuation, counts as sung.
        Some(1.0)
    }

    /// the line with its timing cut to end by `ms`, so the fill is complete then. a held last
    /// word's own timing can run past the time its line stops being current.
    pub fn filled_by(&self, ms: u32) -> Self {
        let mut line = self.clone();
        for (i, w) in line.words.iter_mut().enumerate() {
            let end = w.end_ms.or_else(|| self.words.get(i + 1).map(|n| n.start_ms)).or(self.end_ms).unwrap_or(w.start_ms);
            w.start_ms = w.start_ms.min(ms);
            w.end_ms = Some(end.min(ms));
        }
        line.end_ms = self.end_ms.map(|e| e.min(ms));
        line
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lyrics {
    pub synced: bool,
    pub lines: Vec<LyricLine>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    /// Unix seconds.
    fetched: u64,
    lyrics: Option<Lyrics>,
    /// [`STORED_VERSION`] when written, 0 for files from before word timing.
    #[serde(default)]
    version: u32,
}

/// Version 2 lines may carry word timing. Older synced entries still play but are fetched again once.
const STORED_VERSION: u32 = 2;

/// One account's lyrics on disk.
pub struct LyricsCache {
    dir: PathBuf,
    max_bytes: u64,
    /// Bytes written since the last trim, so the folder isn't listed on every write.
    since_trim: AtomicU64,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl LyricsCache {
    pub fn new(dir: PathBuf, max_bytes: u64) -> Self {
        let _ = fs::create_dir_all(&dir);
        Self { dir, max_bytes, since_trim: AtomicU64::new(u64::MAX / 2) }
    }

    fn path(&self, song_id: &str) -> PathBuf {
        self.dir.join(format!("{:x}.json", md5::compute(song_id.as_bytes())))
    }

    /// What's stored for a song, `None` lyrics when it has none, and whether it's still fresh.
    pub fn get(&self, song_id: &str) -> Option<(Option<Lyrics>, bool)> {
        let path = self.path(song_id);
        let stored: Stored = serde_json::from_slice(&fs::read(&path).ok()?).ok()?;
        if let Ok(file) = fs::File::options().write(true).open(&path) {
            let _ = file.set_modified(SystemTime::now());
        }
        let age = Duration::from_secs(now_secs().saturating_sub(stored.fetched));
        let fresh = age < if stored.lyrics.is_some() { FRESH_FOR } else { NONE_FRESH_FOR };
        let outdated = stored.version < STORED_VERSION && stored.lyrics.as_ref().is_some_and(|l| l.synced);
        // Tidied on the way out too, so entries stored before tags were dropped read the same.
        Some((stored.lyrics.and_then(tidy), fresh && !outdated))
    }

    pub fn put(&self, song_id: &str, lyrics: Option<&Lyrics>) {
        let Ok(text) = serde_json::to_vec(&Stored { fetched: now_secs(), lyrics: lyrics.cloned(), version: STORED_VERSION }) else { return };
        let path = self.path(song_id);
        let tmp = path.with_extension("tmp");
        if fs::write(&tmp, &text).is_ok() && fs::rename(&tmp, &path).is_err() {
            let _ = fs::remove_file(&tmp);
        }
        // Trim when about a tenth of the limit has been written since the last look.
        if self.since_trim.fetch_add(text.len() as u64, Ordering::AcqRel) + text.len() as u64 > self.max_bytes / 10 {
            self.since_trim.store(0, Ordering::Release);
            trim(&self.dir, self.max_bytes);
        }
    }

    /// Bytes on disk.
    pub fn usage(&self) -> u64 {
        files(&self.dir).iter().map(|(_, size, _)| size).sum()
    }

    pub fn clear(&self) {
        for (path, _, _) in files(&self.dir) {
            let _ = fs::remove_file(path);
        }
    }
}

fn files(dir: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            meta.is_file().then(|| (e.path(), meta.len(), meta.modified().unwrap_or(UNIX_EPOCH)))
        })
        .collect()
}

fn trim(dir: &Path, max_bytes: u64) {
    let mut all = files(dir);
    let mut total: u64 = all.iter().map(|(_, size, _)| size).sum();
    if total <= max_bytes {
        return;
    }
    all.sort_by_key(|(_, _, modified)| *modified);
    for (path, size, _) in all {
        if total <= max_bytes {
            break;
        }
        if fs::remove_file(path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

/// A song's lyrics from the cache while fresh, else the server, else whatever the cache has.
/// Only real server answers are stored, never failures. `features` picks the endpoint.
pub async fn lyrics_for(client: &Client, cache: Option<&LyricsCache>, song_id: &str, artist_title: Option<(&str, &str)>, features: Option<ServerFeatures>) -> Option<Lyrics> {
    let stored = cache.and_then(|c| c.get(song_id));
    if let Some((lyrics, true)) = &stored {
        return lyrics.clone();
    }
    let structured = features.is_none_or(|f| f.synced_lyrics);
    let enhanced = features.is_some_and(|f| f.word_lyrics);
    match fetch(client, song_id, artist_title, structured, enhanced).await {
        Ok(found) => {
            if let Some(cache) = cache {
                cache.put(song_id, found.as_ref());
            }
            found
        }
        Err(e) => {
            tracing::debug!("lyrics unavailable from the server: {e}");
            stored.and_then(|(lyrics, _)| lyrics)
        }
    }
}

/// `Ok(None)` means the server has no lyrics, and `Err` means no answer.
async fn fetch(client: &Client, song_id: &str, artist_title: Option<(&str, &str)>, structured: bool, enhanced: bool) -> Result<Option<Lyrics>, Error> {
    let mut failure = None;
    if structured {
        match client.lyrics_by_song_id(song_id, enhanced).await {
            Ok(lists) => {
                if let Some(found) = from_structured(lists) {
                    return Ok(Some(found));
                }
            }
            Err(e) => failure = Some(e),
        }
    }
    // Servers without the extension still answer the classic endpoint, unsynced.
    if let Some((artist, title)) = artist_title {
        match client.lyrics(artist, title).await {
            Ok(plain) => {
                let Some(text) = plain.value.filter(|t| !t.trim().is_empty()) else { return Ok(None) };
                return Ok(tidy(Lyrics { synced: false, lines: text.lines().map(|l| LyricLine::plain(None, l)).collect() }));
            }
            Err(e) => return Err(e),
        }
    }
    match failure {
        // A server without the endpoint has answered, with nothing.
        Some(e) if e.is_transient() => Err(e),
        _ => Ok(None),
    }
}

/// The lyrics to show from `getLyricsBySongId`, synced where possible and [`tidy`]ed.
/// Translations and pronunciations are never picked.
pub fn from_structured(lists: Vec<StructuredLyrics>) -> Option<Lyrics> {
    let main = |l: &StructuredLyrics| l.kind.as_deref().is_none_or(|k| k.eq_ignore_ascii_case("main")) && !l.line.is_empty();
    // Lines without real timing can't follow the song, whatever the flag says.
    let synced = |l: &StructuredLyrics| l.synced.unwrap_or(true) && l.line.iter().any(|x| x.start.unwrap_or(0) > 0);
    let pick = lists.iter().position(|l| main(l) && synced(l)).or_else(|| lists.iter().position(main))?;
    let list = lists.into_iter().nth(pick)?;
    let is_synced = synced(&list);
    let offset = list.offset.unwrap_or(0);
    let at = |ms: i64| (ms + offset).max(0) as u32;
    let lead = list.agents.iter().find(|a| a.role.as_deref() == Some("main")).map(|a| a.id.clone());
    let mut lines: Vec<LyricLine> = list
        .line
        .iter()
        .map(|l| LyricLine {
            start_ms: is_synced.then(|| at(l.start.unwrap_or(0))),
            text: l.value.clone(),
            end_ms: if is_synced { l.end.map(at) } else { None },
            words: Vec::new(),
        })
        .collect();
    if is_synced {
        for (i, line) in lines.iter_mut().enumerate() {
            let mut cues = list.cue_line.iter().filter(|c| c.index == Some(i as i64)).peekable();
            let first = cues.peek().copied();
            // The lead vocal's cue line when the entry names agents, else the first one.
            let chosen = cues.find(|c| lead.is_some() && c.agent_id == lead).or(first);
            if let Some(cue_line) = chosen {
                line.words = words_for(&line.text, cue_line, &at);
            }
        }
    }
    tidy(Lyrics { synced: is_synced, lines })
}

/// Whether a whole line is an LRC ID tag such as `[ar:...]` or `[offset:...]`.
/// Navidrome keeps these as lines in unsynced text, so they're dropped here for every source.
fn is_id_tag(line: &str) -> bool {
    let Some(inner) = line.trim().strip_prefix('[').and_then(|l| l.strip_suffix(']')) else { return false };
    let Some((name, _)) = inner.split_once(':') else { return false };
    name.starts_with(|c: char| c.is_alphabetic() || c == '#') && !inner.contains(['[', ']'])
}

/// Whether a line only says the song has no vocals, like "instrumental" in any case or brackets.
fn is_instrumental_mark(line: &str) -> bool {
    line.trim_matches(|c: char| c.is_whitespace() || "[]()*{}<>-_.:".contains(c)).eq_ignore_ascii_case("instrumental")
}

/// Drops LRC ID tags, and returns `None` when no real line is left.
/// The apps then show their no-lyrics state rather than a tag or a lone "[Instrumental]".
pub fn tidy(mut lyrics: Lyrics) -> Option<Lyrics> {
    lyrics.lines.retain(|l| !is_id_tag(&l.text));
    let real = |l: &LyricLine| l.text.chars().any(char::is_alphanumeric) && !is_instrumental_mark(&l.text);
    if !lyrics.lines.iter().any(real) {
        return None;
    }
    // Blank lines at the very start would push the first line down for nothing.
    let first = lyrics.lines.iter().position(|l| !l.text.trim().is_empty()).unwrap_or(0);
    if !lyrics.synced {
        lyrics.lines.drain(..first);
    }
    Some(lyrics)
}

/// A cue line's cues as byte ranges of the shown line.
/// Anything that can't be placed exactly is dropped, and a line left without words falls back to the estimate.
fn words_for(text: &str, cue_line: &CueLine, at: &impl Fn(i64) -> u32) -> Vec<LyricWord> {
    let value = cue_line.value.as_deref().unwrap_or(text);
    let Some(base) = (if value == text { Some(0) } else { text.find(value) }) else { return Vec::new() };
    let mut words = Vec::new();
    let mut last_end = 0u32;
    for cue in &cue_line.cue {
        let (Some(start), Some(bs), Some(be)) = (cue.start, cue.byte_start, cue.byte_end) else { continue };
        if bs < 0 || be < bs || be as usize >= value.len() {
            continue;
        }
        // The cue's end is inclusive, so widen it to the end of that character.
        let s = bs as usize;
        let mut e = be as usize + 1;
        while e < value.len() && !value.is_char_boundary(e) {
            e += 1;
        }
        if !value.is_char_boundary(s) {
            continue;
        }
        let (s, e) = ((base + s) as u32, (base + e) as u32);
        // Kept in order and without overlaps, so the fill only ever moves forward.
        if s < last_end {
            continue;
        }
        last_end = e;
        words.push(LyricWord { start: s, end: e, start_ms: at(start), end_ms: cue.end.map(at) });
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_reads_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf(), DEFAULT_MAX_BYTES);
        let lyrics = Lyrics { synced: true, lines: vec![LyricLine::plain(Some(1200), "hello")] };
        cache.put("a", Some(&lyrics));
        cache.put("b", None);
        assert_eq!(cache.get("a"), Some((Some(lyrics), true)));
        assert_eq!(cache.get("b"), Some((None, true)));
        assert!(cache.get("c").is_none());
        assert!(cache.usage() > 0);
        cache.clear();
        assert_eq!(cache.usage(), 0);
    }

    #[test]
    fn entries_from_before_word_timing_read_but_are_asked_again() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf(), DEFAULT_MAX_BYTES);
        let old = |synced: bool| format!(r#"{{"fetched":{},"lyrics":{{"synced":{synced},"lines":[{{"start_ms":10,"text":"hi"}}]}}}}"#, now_secs());
        fs::write(cache.path("synced"), old(true)).unwrap();
        fs::write(cache.path("plain"), old(false)).unwrap();
        let (lyrics, fresh) = cache.get("synced").unwrap();
        assert_eq!((lyrics.unwrap().lines[0].text.as_str(), fresh), ("hi", false));
        assert!(cache.get("plain").unwrap().1, "unsynced lyrics have no words to gain");
        cache.put("synced", Some(&Lyrics { synced: true, lines: vec![LyricLine::plain(Some(10), "hi")] }));
        assert!(cache.get("synced").unwrap().1);
    }

    #[test]
    fn a_held_word_cut_to_the_hold_is_filled_by_then() {
        // "EVEN BETTER": the last word's timing runs to 59670, the line stops being current at 59073
        let word = |start: u32, end: u32, start_ms: u32, end_ms: Option<u32>| LyricWord { start, end, start_ms, end_ms };
        let line = LyricLine { start_ms: Some(57000), text: "EVEN BETTER".into(), end_ms: None, words: vec![word(0, 4, 57000, None), word(5, 11, 58459, Some(59670))] };
        assert!(line.sung_fraction(58773).unwrap() < 0.8, "uncut, the glow would start fading over unlit letters");
        let cut = line.filled_by(58773);
        assert_eq!(cut.sung_fraction(58773), Some(1.0));
        assert_eq!(cut.sung_fraction(57000), Some(0.0));
        assert_eq!(cut.words[0].end_ms, Some(58459), "an earlier word keeps its own timing");
        assert_eq!(cut.sung_fraction(58500).unwrap() > line.sung_fraction(58500).unwrap(), true, "the held word fills faster, never backwards");
    }

    fn plain(lines: &[&str]) -> Lyrics {
        Lyrics { synced: false, lines: lines.iter().map(|l| LyricLine::plain(None, *l)).collect() }
    }

    fn texts(l: &Lyrics) -> Vec<&str> {
        l.lines.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn tidy_drops_id_tags_and_keeps_lyrics() {
        let l = tidy(plain(&["[ar: Someone]", "[ti:Song]", "[al: Album]", "[by: x]", "[offset:+120]", "[length: 03:12]", "[re:tool]", "[ve:1.0]", "[#: a comment]", "", "First line", "[Chorus]", "Second [ad lib] line", "[01:02.30]"])).unwrap();
        assert_eq!(texts(&l), ["First line", "[Chorus]", "Second [ad lib] line", "[01:02.30]"]);
        let synced = Lyrics { synced: true, lines: vec![LyricLine::plain(Some(0), "[au: someone]"), LyricLine::plain(Some(500), "Hello"), LyricLine::plain(Some(900), "(Instrumental)")] };
        assert_eq!(texts(&tidy(synced).unwrap()), ["Hello", "(Instrumental)"]);
    }

    #[test]
    fn tidy_leaves_nothing_for_instrumental_marks() {
        for only in [&["[au: instrumental]"][..], &["[Instrumental]"], &["(instrumental)"], &["Instrumental"], &["*instrumental*", ""], &["-"], &["[ar: x]", "   "]] {
            assert_eq!(tidy(plain(only)), None, "{only:?}");
        }
        assert!(tidy(plain(&["Instrumental break", "la la"])).is_some());
    }

    #[test]
    fn structured_tags_and_instrumental_from_the_server() {
        let list = |synced: bool, lines: &[(Option<i64>, &str)]| StructuredLyrics {
            synced: Some(synced),
            line: lines.iter().map(|(s, v)| crate::client::LyricLine { start: *s, end: None, value: v.to_string() }).collect(),
            ..Default::default()
        };
        // Navidrome's answer for an unsynced file holding just an ID tag.
        assert_eq!(from_structured(vec![list(false, &[(None, "[au: instrumental]")])]), None);
        let l = from_structured(vec![list(false, &[(None, "[ar: A]"), (None, "Line one"), (None, "Line two")])]).unwrap();
        assert_eq!(texts(&l), ["Line one", "Line two"]);
    }

    #[test]
    fn stored_entries_read_tidied() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf(), DEFAULT_MAX_BYTES);
        cache.put("tag", Some(&plain(&["[au: instrumental]"])));
        assert_eq!(cache.get("tag").map(|(l, _)| l), Some(None));
    }

    #[test]
    fn trims_to_its_limit() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf(), 2_000);
        let lyrics = Lyrics { synced: false, lines: vec![LyricLine::plain(None, "x".repeat(400))] };
        for i in 0..20 {
            cache.put(&i.to_string(), Some(&lyrics));
        }
        assert!(cache.usage() <= 2_000, "{}", cache.usage());
    }
}
