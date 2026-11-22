//! Fuzzy, ranked local search over artists, albums and songs, with an in-memory word index per kind
//! kept current through the writer's update hook. Ranking goes by match quality, then field, then
//! popularity, then length, so a fuzzy match never outranks an exact one.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OpenFlags, params_from_iter};
use serde::Deserialize;

use super::text::{is_cjk_word, search_words};
use crate::client::{Album, Song};
use crate::error::{Error, Result};

/// Which list a result comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SearchKind {
    Artist,
    Album,
    Song,
}

const KINDS: [SearchKind; 3] = [SearchKind::Artist, SearchKind::Album, SearchKind::Song];

impl SearchKind {
    fn index(self) -> usize {
        match self {
            SearchKind::Artist => 0,
            SearchKind::Album => 1,
            SearchKind::Song => 2,
        }
    }

    fn table(self) -> &'static str {
        match self {
            SearchKind::Artist => "artist",
            SearchKind::Album => "album",
            SearchKind::Song => "song",
        }
    }

    fn from_table(table: &str) -> Option<Self> {
        match table {
            "artist" => Some(SearchKind::Artist),
            "album" => Some(SearchKind::Album),
            "song" => Some(SearchKind::Song),
            _ => None,
        }
    }

    /// Importance of each field, in the order the fields are indexed.
    fn weights(self) -> &'static [u16] {
        match self {
            SearchKind::Artist => &[8],
            SearchKind::Album => &[8, 3],
            SearchKind::Song => &[8, 4, 2, 1],
        }
    }
}

/// The text a song is found by: title, artists, album, album artists.
pub(crate) fn song_fields(s: &Song) -> [String; 4] {
    [
        s.title.clone(),
        joined(s.display_artist.as_deref().or(s.artist.as_deref()), s.artists.iter().map(|x| x.name.as_str())),
        s.album.clone().unwrap_or_default(),
        joined(s.display_album_artist.as_deref(), s.album_artists.iter().map(|x| x.name.as_str())),
    ]
}

/// The text an album is found by: name, artists.
pub(crate) fn album_fields(a: &Album) -> [String; 2] {
    [a.name.clone(), joined(a.display_artist.as_deref().or(a.artist.as_deref()), a.artists.iter().map(|x| x.name.as_str()))]
}

pub(crate) fn joined<'a>(first: Option<&'a str>, rest: impl Iterator<Item = &'a str>) -> String {
    let mut out = String::new();
    if let Some(f) = first {
        out.push_str(f);
    }
    for name in rest {
        if !out.contains(name) {
            out.push(' ');
            out.push_str(name);
        }
    }
    out
}

// ---- limits -------------------------------------------------------------------------------

/// Query words looked at; the rest of a very long query is ignored.
const MAX_QUERY_WORDS: usize = 10;
/// Words of a field that get a position; later words share the last one.
const MAX_POSITION: usize = u8::MAX as usize;
/// Shortest query word matched inside other words. Scripts without short words use a lower limit.
const MIN_INFIX_ASCII: usize = 3;
const MIN_INFIX_OTHER: usize = 2;
/// Longest query word compared by edit distance.
const MAX_FUZZY_CHARS: usize = 48;

/// Typos forgiven for a query word of this many characters.
fn max_edits(chars: usize) -> u8 {
    match chars {
        0..=3 => 0,
        4..=7 => 1,
        _ => 2,
    }
}

// ---- the index ----------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Posting {
    doc: u32,
    field: u8,
    pos: u8,
}

struct Term {
    text: Box<str>,
    chars: u16,
    ascii: bool,
    mask: u64,
    postings: Vec<Posting>,
}

struct Doc {
    rowid: i64,
    alive: bool,
    /// Interned library id, `u16::MAX` for artists (which can be in several).
    library: u16,
    plays: i64,
    starred: bool,
    /// `created` as digits, for "recently added".
    recency: i64,
    content_hash: u64,
    /// The fields' words: words joined by ' ', fields by '\n'.
    text: Box<str>,
    /// Albums: the album artist's id. Artists: their own id.
    owner: Box<str>,
    /// Artists: albums credited to them.
    albums: i64,
    /// Length of the first field's words, for "shorter names first".
    name_len: u16,
}

/// One document's fields, ready to index.
struct Row {
    rowid: i64,
    library: Option<String>,
    plays: i64,
    starred: bool,
    created: Option<String>,
    content_hash: String,
    owner: String,
    albums: i64,
    fields: Vec<String>,
}

/// Field text to its words' term ids and the words joined, while building.
type FieldCache = HashMap<String, (Vec<u32>, String)>;
const FIELD_CACHE_MAX: usize = 100_000;

#[derive(Default)]
struct KindIndex {
    docs: Vec<Doc>,
    by_rowid: HashMap<i64, u32>,
    terms: Vec<Term>,
    vocab: BTreeMap<Box<str>, u32>,
    dead: usize,
}

fn char_bit(c: char) -> u64 {
    match c {
        'a'..='z' => 1 << (c as u32 - 'a' as u32),
        '0'..='9' => 1 << (26 + c as u32 - '0' as u32),
        other => 1 << (36 + other as u32 % 28),
    }
}

fn mask_of(word: &str) -> u64 {
    word.chars().fold(0, |m, c| m | char_bit(c))
}

fn hash64(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

fn recency_of(created: Option<&str>) -> i64 {
    created.map(|c| c.chars().filter(char::is_ascii_digit).take(14).fold(0i64, |n, d| n * 10 + (d as i64 - '0' as i64))).unwrap_or(0)
}

impl KindIndex {
    fn live(&self) -> usize {
        self.docs.len() - self.dead
    }

    fn term_id(&mut self, word: &str) -> u32 {
        if let Some(id) = self.vocab.get(word) {
            return *id;
        }
        let id = self.terms.len() as u32;
        self.terms.push(Term { text: word.into(), chars: word.chars().count().min(u16::MAX as usize) as u16, ascii: word.is_ascii(), mask: mask_of(word), postings: Vec::new() });
        self.vocab.insert(word.into(), id);
        id
    }

    fn insert(&mut self, row: Row, libraries: &mut Libraries, cache: Option<&mut FieldCache>) {
        if let Some(old) = self.by_rowid.get(&row.rowid).copied() {
            self.kill(old);
        }
        let doc = self.docs.len() as u32;
        let mut text = String::new();
        let mut local = FieldCache::default();
        let cache = cache.unwrap_or(&mut local);
        for (f, field) in row.fields.iter().enumerate() {
            if f > 0 {
                text.push('\n');
            }
            // Artists and albums repeat on every song, so a build tokenizes each text once.
            if !cache.contains_key(field.as_str()) {
                let words = search_words(field);
                let ids = words.iter().map(|w| self.term_id(w)).collect();
                cache.insert(field.clone(), (ids, words.join(" ")));
            }
            let (ids, joined) = &cache[field.as_str()];
            text.push_str(joined);
            for (pos, id) in ids.iter().enumerate() {
                self.terms[*id as usize].postings.push(Posting { doc, field: f as u8, pos: pos.min(MAX_POSITION) as u8 });
            }
        }
        if cache.len() > FIELD_CACHE_MAX {
            cache.clear();
        }
        let name_len = text.split('\n').next().unwrap_or("").len().min(u16::MAX as usize) as u16;
        self.docs.push(Doc {
            name_len,
            rowid: row.rowid,
            alive: true,
            library: row.library.as_deref().map(|l| libraries.intern(l)).unwrap_or(u16::MAX),
            plays: row.plays,
            starred: row.starred,
            recency: recency_of(row.created.as_deref()),
            content_hash: hash64(&row.content_hash),
            text: text.into(),
            owner: row.owner.into(),
            albums: row.albums,
        });
        self.by_rowid.insert(row.rowid, doc);
    }

    fn kill(&mut self, doc: u32) {
        let d = &mut self.docs[doc as usize];
        if d.alive {
            d.alive = false;
            self.dead += 1;
            self.by_rowid.remove(&d.rowid);
        }
    }

    /// Drops dead documents and their postings once they are a large share.
    fn compact_if_needed(&mut self) {
        if self.dead < 1024 || self.dead * 2 < self.docs.len() {
            return;
        }
        let mut remap = vec![u32::MAX; self.docs.len()];
        let mut docs = Vec::with_capacity(self.live());
        for (i, d) in std::mem::take(&mut self.docs).into_iter().enumerate() {
            if d.alive {
                remap[i] = docs.len() as u32;
                docs.push(d);
            }
        }
        self.docs = docs;
        self.by_rowid = self.docs.iter().enumerate().map(|(i, d)| (d.rowid, i as u32)).collect();
        for term in &mut self.terms {
            term.postings.retain_mut(|p| {
                p.doc = remap[p.doc as usize];
                p.doc != u32::MAX
            });
        }
        self.dead = 0;
    }
}

#[derive(Default)]
struct Libraries {
    ids: Vec<String>,
}

impl Libraries {
    fn intern(&mut self, id: &str) -> u16 {
        if let Some(i) = self.ids.iter().position(|x| x == id) {
            return i as u16;
        }
        self.ids.push(id.to_string());
        (self.ids.len() - 1).min(u16::MAX as usize - 1) as u16
    }

    fn find(&self, id: &str) -> Option<u16> {
        self.ids.iter().position(|x| x == id).map(|i| i as u16)
    }
}

#[derive(Default)]
struct Indexes {
    kinds: [KindIndex; 3],
    scratch: Scratch,
    libraries: Libraries,
    /// Plays of each album artist's albums, for artist popularity.
    artist_plays: HashMap<Box<str>, i64>,
}

impl Indexes {
    fn insert(&mut self, kind: SearchKind, row: Row, cache: Option<&mut FieldCache>) {
        if kind == SearchKind::Album {
            self.forget_album_plays(row.rowid);
            *self.artist_plays.entry(row.owner.as_str().into()).or_default() += row.plays;
        }
        self.kinds[kind.index()].insert(row, &mut self.libraries, cache);
    }

    fn remove(&mut self, kind: SearchKind, rowid: i64) {
        if kind == SearchKind::Album {
            self.forget_album_plays(rowid);
        }
        let k = &mut self.kinds[kind.index()];
        if let Some(doc) = k.by_rowid.get(&rowid).copied() {
            k.kill(doc);
        }
    }

    fn forget_album_plays(&mut self, rowid: i64) {
        let k = &self.kinds[SearchKind::Album.index()];
        if let Some(d) = k.by_rowid.get(&rowid).map(|i| &k.docs[*i as usize])
            && let Some(p) = self.artist_plays.get_mut(&d.owner)
        {
            *p -= d.plays;
        }
    }
}

// ---- reading rows -------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct SongText {
    title: String,
    artist: Option<String>,
    display_artist: Option<String>,
    artists: Vec<crate::client::ArtistRef>,
    album: Option<String>,
    display_album_artist: Option<String>,
    album_artists: Vec<crate::client::ArtistRef>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct AlbumText {
    name: String,
    artist: Option<String>,
    display_artist: Option<String>,
    artists: Vec<crate::client::ArtistRef>,
}

fn row_sql(kind: SearchKind) -> &'static str {
    match kind {
        SearchKind::Song => "SELECT rowid, library_id, play_count, starred IS NOT NULL, created, content_hash, '', 0, data FROM song",
        SearchKind::Album => "SELECT rowid, library_id, play_count, starred IS NOT NULL, created, content_hash, ifnull(artist_id, ''), 0, data FROM album",
        SearchKind::Artist => "SELECT rowid, NULL, 0, starred IS NOT NULL, NULL, content_hash, id, album_count, name FROM artist",
    }
}

fn read_row(kind: SearchKind, r: &rusqlite::Row) -> rusqlite::Result<Row> {
    let data: String = r.get(8)?;
    let fields = match kind {
        SearchKind::Song => {
            let s: SongText = serde_json::from_str(&data).unwrap_or_default();
            let song = Song { title: s.title, artist: s.artist, display_artist: s.display_artist, artists: s.artists, album: s.album, display_album_artist: s.display_album_artist, album_artists: s.album_artists, ..Default::default() };
            song_fields(&song).to_vec()
        }
        SearchKind::Album => {
            let a: AlbumText = serde_json::from_str(&data).unwrap_or_default();
            let album = Album { name: a.name, artist: a.artist, display_artist: a.display_artist, artists: a.artists, ..Default::default() };
            album_fields(&album).to_vec()
        }
        SearchKind::Artist => vec![data],
    };
    Ok(Row {
        rowid: r.get(0)?,
        library: r.get(1)?,
        plays: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
        starred: r.get(3)?,
        created: r.get(4)?,
        content_hash: r.get(5)?,
        owner: r.get(6)?,
        albums: r.get::<_, Option<i64>>(7)?.unwrap_or(0),
        fields,
    })
}

fn build(path: &std::path::Path) -> Result<Indexes> {
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    let mut ix = Indexes::default();
    // Albums before artists, so artist popularity is complete. One snapshot for all three.
    c.execute_batch("BEGIN")?;
    for kind in [SearchKind::Album, SearchKind::Artist, SearchKind::Song] {
        let mut st = c.prepare(row_sql(kind))?;
        let mut cache = FieldCache::default();
        for row in st.query_map([], |r| read_row(kind, r))? {
            ix.insert(kind, row?, Some(&mut cache));
        }
    }
    c.execute_batch("COMMIT")?;
    Ok(ix)
}

// ---- shared state -------------------------------------------------------------------------

/// Rows changed by the writer, recorded by the update hook until their transaction commits.
#[derive(Default)]
pub(crate) struct Changes {
    pending: Vec<(SearchKind, i64)>,
    committed: HashSet<(SearchKind, i64)>,
}

#[derive(Default)]
pub(crate) struct SearchState {
    index: Mutex<Option<Indexes>>,
    changes: Arc<Mutex<Changes>>,
}

impl SearchState {
    /// Records changed rows on the writer connection.
    pub(crate) fn watch(&self, writer: &mut Connection) -> Result<()> {
        let changes = self.changes.clone();
        writer.update_hook(Some(move |_action, _db: &str, table: &str, rowid: i64| {
            if let Some(kind) = SearchKind::from_table(table) {
                changes.lock().expect("search changes").pending.push((kind, rowid));
            }
        }))?;
        Ok(())
    }

    /// Called after the writer's transaction ended. Keeps its changes if it committed.
    pub(crate) fn finish_write(&self, committed: bool) {
        let mut c = self.changes.lock().expect("search changes");
        let pending = std::mem::take(&mut c.pending);
        if committed {
            c.committed.extend(pending);
        }
    }

    /// Builds the index now, so the first search doesn't wait for it.
    pub(crate) fn warm(&self, path: &std::path::Path) -> Result<()> {
        let mut guard = self.index.lock().expect("search index");
        if guard.is_none() {
            *guard = Some(build(path)?);
        }
        Ok(())
    }
}

/// One ranked result: the row to load and how it matched.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Hit {
    pub rowid: i64,
    key: RankKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct RankKey {
    missed: u8,
    tier: u8,
    edits: u8,
    field: std::cmp::Reverse<u16>,
    exact_words: std::cmp::Reverse<u8>,
    popularity: std::cmp::Reverse<u32>,
    length: u16,
    recency: std::cmp::Reverse<i64>,
    rowid: i64,
}

const TIER_EXACT: u8 = 0;
const TIER_PREFIX: u8 = 1;
const TIER_WORDS: u8 = 2;
const TIER_FUZZY: u8 = 3;
const TIER_INFIX: u8 = 4;

impl Hit {
    /// How this hit compares with the best of other kinds. A match on a song's artist or album
    /// field counts as a word match, so the artist or album itself leads.
    fn cross_kind(&self, kind: SearchKind) -> (u8, u8, u8, usize) {
        let k = self.key;
        let tier = if k.tier <= TIER_PREFIX && k.field.0 < kind.weights()[0] { TIER_WORDS } else { k.tier };
        (k.missed, tier, k.edits, kind.index())
    }
}

/// The best results of each kind, best first, and which kind holds the overall best.
pub(crate) struct Ranked {
    pub hits: [Vec<Hit>; 3],
    pub best: Option<SearchKind>,
}

impl SearchState {
    pub(crate) fn search(&self, path: &std::path::Path, reader: &Connection, query: &str, limit: usize, libraries: Option<&[String]>) -> Result<Ranked> {
        let mut guard = self.index.lock().expect("search index");
        if guard.is_none() {
            *guard = Some(build(path)?);
        }
        let ix = guard.as_mut().expect("built");
        self.apply_changes(ix, reader)?;
        let q = Query::parse(query);
        let mut hits: [Vec<Hit>; 3] = Default::default();
        if q.words.is_empty() {
            return Ok(Ranked { hits, best: None });
        }
        let artist_rowids: Option<HashSet<i64>> = match libraries {
            Some(ids) if !ids.is_empty() => {
                let sql = format!("SELECT artist_rowid FROM artist_library WHERE library_id IN ({})", vec!["?"; ids.len()].join(","));
                let mut st = reader.prepare_cached(&sql)?;
                let rows = st.query_map(params_from_iter(ids.iter()), |r| r.get::<_, i64>(0))?;
                Some(rows.collect::<std::result::Result<_, _>>()?)
            }
            Some(_) => Some(HashSet::new()),
            None => None,
        };
        let wanted_libraries: Option<HashSet<u16>> = libraries.map(|ids| ids.iter().filter_map(|id| ix.libraries.find(id)).collect());
        for kind in KINDS {
            let Indexes { kinds, scratch, artist_plays, .. } = &mut *ix;
            let k = &kinds[kind.index()];
            let allowed = |d: &Doc| match kind {
                SearchKind::Artist => artist_rowids.as_ref().is_none_or(|set| set.contains(&d.rowid)),
                _ => wanted_libraries.as_ref().is_none_or(|set| set.contains(&d.library)),
            };
            hits[kind.index()] = rank(k, scratch, kind, &q, limit, allowed, |d| popularity(kind, d, artist_plays));
        }
        // Words missing from some results are only worth showing when nothing has them all.
        if hits.iter().any(|h| h.first().is_some_and(|h| h.key.missed == 0)) {
            for h in &mut hits {
                h.retain(|h| h.key.missed == 0);
            }
        }
        let best = KINDS.into_iter().filter_map(|kind| hits[kind.index()].first().map(|h| (h.cross_kind(kind), kind))).min_by_key(|(key, _)| *key).map(|(_, kind)| kind);
        Ok(Ranked { hits, best })
    }

    fn apply_changes(&self, ix: &mut Indexes, reader: &Connection) -> Result<()> {
        let changed = std::mem::take(&mut self.changes.lock().expect("search changes").committed);
        if changed.is_empty() {
            return Ok(());
        }
        for kind in KINDS {
            let rowids: Vec<i64> = changed.iter().filter(|(k, _)| *k == kind).map(|(_, r)| *r).collect();
            for chunk in rowids.chunks(500) {
                let marks = vec!["?"; chunk.len()].join(",");
                // A sync stamps every row it sees, so most changed rows have the same text.
                // Check the content hash first and re-read only the rest.
                let light = format!("SELECT rowid, content_hash, {} FROM {} WHERE rowid IN ({marks})", POPULARITY_COLUMNS[kind.index()], kind.table());
                let mut reread: Vec<i64> = Vec::new();
                let mut seen = HashSet::new();
                {
                    let mut st = reader.prepare(&light)?;
                    let mut rows = st.query(params_from_iter(chunk.iter()))?;
                    while let Some(r) = rows.next()? {
                        let rowid: i64 = r.get(0)?;
                        seen.insert(rowid);
                        let hash = hash64(&r.get::<_, String>(1)?);
                        let (plays, starred, albums): (Option<i64>, bool, Option<i64>) = (r.get(2)?, r.get(3)?, r.get(4)?);
                        let k = &mut ix.kinds[kind.index()];
                        match k.by_rowid.get(&rowid).copied().filter(|d| k.docs[*d as usize].content_hash == hash) {
                            Some(doc) => {
                                let d = &mut k.docs[doc as usize];
                                let plays = plays.unwrap_or(0);
                                if kind == SearchKind::Album {
                                    *ix.artist_plays.entry(d.owner.clone()).or_default() += plays - d.plays;
                                }
                                d.plays = plays;
                                d.starred = starred;
                                d.albums = albums.unwrap_or(0);
                            }
                            None => reread.push(rowid),
                        }
                    }
                }
                if !reread.is_empty() {
                    let sql = format!("{} WHERE rowid IN ({})", row_sql(kind), vec!["?"; reread.len()].join(","));
                    let mut st = reader.prepare(&sql)?;
                    for row in st.query_map(params_from_iter(reread.iter()), |r| read_row(kind, r))? {
                        ix.insert(kind, row?, None);
                    }
                }
                for rowid in chunk.iter().filter(|r| !seen.contains(r)) {
                    ix.remove(kind, *rowid);
                }
            }
            ix.kinds[kind.index()].compact_if_needed();
        }
        Ok(())
    }
}

/// Plays, starred and album count, in `row_sql`'s order, for the hash-only check.
const POPULARITY_COLUMNS: [&str; 3] = ["0, starred IS NOT NULL, album_count", "play_count, starred IS NOT NULL, 0", "play_count, starred IS NOT NULL, 0"];

fn plays_bucket(plays: i64) -> u32 {
    64 - (plays.max(0) as u64).leading_zeros()
}

fn popularity(kind: SearchKind, d: &Doc, artist_plays: &HashMap<Box<str>, i64>) -> u32 {
    let plays = match kind {
        SearchKind::Artist => artist_plays.get(&d.owner).copied().unwrap_or(0) + d.albums,
        _ => d.plays,
    };
    plays_bucket(plays) + if d.starred { 4 } else { 0 }
}

// ---- queries ------------------------------------------------------------------------------

struct Query {
    words: Vec<String>,
    /// The query without spaces, compared with whole fields.
    joined: String,
    /// Runs of two or more CJK characters, which must appear together.
    cjk_runs: Vec<String>,
}

impl Query {
    fn parse(input: &str) -> Self {
        let mut words = search_words(input);
        words.truncate(MAX_QUERY_WORDS);
        let joined = words.concat();
        let mut cjk_runs = Vec::new();
        let mut run = String::new();
        for w in &words {
            if is_cjk_word(w) {
                run.push_str(w);
            } else {
                if run.chars().count() > 1 {
                    cjk_runs.push(run.clone());
                }
                run.clear();
            }
        }
        if run.chars().count() > 1 {
            cjk_runs.push(run);
        }
        Self { words, joined, cjk_runs }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Match {
    Infix = 1,
    Fuzzy = 2,
    Prefix = 3,
    Exact = 4,
}

#[derive(Clone, Copy)]
struct TermMatch {
    term: u32,
    kind: Match,
    edits: u8,
}

/// Optimal string alignment distance from `q` to all of `t` and to `t`'s closest start, each
/// `None` past `k`.
fn osa<T: PartialEq + Copy>(q: &[T], t: &[T], k: u8, rows: &mut [Vec<u8>; 3]) -> (Option<u8>, Option<u8>) {
    let m = q.len();
    let n = t.len().min(m + k as usize);
    for r in rows.iter_mut() {
        r.clear();
        r.resize(n + 1, 0);
    }
    for (j, v) in rows[0].iter_mut().enumerate() {
        *v = j as u8;
    }
    for i in 1..=m {
        let (cur, prev, prev2) = (i % 3, (i + 2) % 3, (i + 1) % 3);
        rows[cur][0] = i as u8;
        let mut row_min = rows[cur][0];
        for j in 1..=n {
            let cost = u8::from(q[i - 1] != t[j - 1]);
            let mut v = (rows[prev][j] + 1).min(rows[cur][j - 1] + 1).min(rows[prev][j - 1] + cost);
            if i > 1 && j > 1 && q[i - 1] == t[j - 2] && q[i - 2] == t[j - 1] {
                v = v.min(rows[prev2][j - 2] + 1);
            }
            rows[cur][j] = v;
            row_min = row_min.min(v);
        }
        if row_min > k {
            return (None, None);
        }
    }
    let last = &rows[m % 3];
    let full = (t.len() <= n).then(|| last[n]).filter(|d| *d <= k);
    let prefix = last[1..].iter().copied().min().filter(|d| *d <= k);
    (full, prefix)
}

/// Every vocabulary word `word` matches, with how. Only `loose` forgives typos or looks inside
/// words. `typing` also forgives typos against a word's start.
fn match_terms(k: &KindIndex, word: &str, loose: bool, typing: bool) -> Vec<TermMatch> {
    let mut out = Vec::new();
    if is_cjk_word(word) {
        if let Some(id) = k.vocab.get(word) {
            out.push(TermMatch { term: *id, kind: Match::Exact, edits: 0 });
        }
        return out;
    }
    let chars: Vec<char> = word.chars().collect();
    let m = chars.len();
    // Very long words are never typos and would overflow the `u8` distances. Numbers never fuzz:
    // "1999" is not a typo of "1989".
    let loose = loose && !word.bytes().all(|b| b.is_ascii_digit());
    let edits = if m > MAX_FUZZY_CHARS || !loose { 0 } else { max_edits(m) };
    let min_infix = if word.is_ascii() { MIN_INFIX_ASCII } else { MIN_INFIX_OTHER };
    let infix = loose && m >= min_infix;
    if edits == 0 && !infix {
        // Short words: exact and prefix only, straight from the sorted vocabulary.
        for (text, id) in k.vocab.range::<str, _>((std::ops::Bound::Included(word), std::ops::Bound::Unbounded)) {
            if !text.starts_with(word) {
                break;
            }
            out.push(TermMatch { term: *id, kind: if &**text == word { Match::Exact } else { Match::Prefix }, edits: 0 });
        }
        return out;
    }
    let mask = mask_of(word);
    let bits: Vec<u64> = chars.iter().map(|c| char_bit(*c)).collect();
    // Each query letter the word lacks costs an edit, repeats included.
    let few_missing = |t: &Term| (mask & !t.mask).count_ones() <= edits as u32 && bits.iter().filter(|b| t.mask & **b == 0).count() <= edits as usize;
    let mut rows: [Vec<u8>; 3] = Default::default();
    let mut tbuf: Vec<char> = Vec::new();
    for (id, t) in k.terms.iter().enumerate() {
        let text = &*t.text;
        let kind = if text == word {
            Some((Match::Exact, 0))
        } else if text.starts_with(word) {
            Some((Match::Prefix, 0))
        } else {
            let fuzzy = if edits > 0 && (t.chars as usize) + (edits as usize) >= m && few_missing(t) {
                let (full, prefix) = if word.is_ascii() && t.ascii {
                    osa(word.as_bytes(), text.as_bytes(), edits, &mut rows)
                } else {
                    tbuf.clear();
                    tbuf.extend(text.chars());
                    osa(&chars, &tbuf, edits, &mut rows)
                };
                full.or(prefix.filter(|_| typing)).map(|d| (Match::Fuzzy, d))
            } else {
                None
            };
            fuzzy.or_else(|| (infix && text.contains(word)).then_some((Match::Infix, 0)))
        };
        if let Some((kind, e)) = kind {
            out.push(TermMatch { term: id as u32, kind, edits: e });
        }
    }
    out
}

/// What one query word scored in one document.
#[derive(Clone, Copy, Default)]
struct SlotHit {
    kind: u8,
    edits: u8,
    weight: u16,
}

impl SlotHit {
    fn key(self) -> (u8, std::cmp::Reverse<u8>, u16) {
        (self.kind, std::cmp::Reverse(self.edits), self.weight)
    }
}

/// Reused between searches: where each document's hits are in `found`, and the hits.
#[derive(Default)]
struct Scratch {
    slot_of: Vec<u32>,
    found: Vec<(u32, [SlotHit; MAX_QUERY_WORDS])>,
}

#[allow(clippy::too_many_arguments)]
fn rank(k: &KindIndex, scratch: &mut Scratch, kind: SearchKind, q: &Query, limit: usize, allowed: impl Fn(&Doc) -> bool, popularity: impl Fn(&Doc) -> u32) -> Vec<Hit> {
    let slots = q.words.len();
    let weights = kind.weights();
    // Per document: the best hit for each query word.
    let Scratch { slot_of, found } = scratch;
    slot_of.resize(k.docs.len(), 0);
    found.clear();
    let mut note = |doc: u32, slot: usize, hit: SlotHit| {
        let at = &mut slot_of[doc as usize];
        if *at == 0 {
            found.push((doc, [SlotHit::default(); MAX_QUERY_WORDS]));
            *at = found.len() as u32;
        }
        let entry = &mut found[*at as usize - 1].1;
        if hit.key() > entry[slot].key() {
            entry[slot] = hit;
        }
    };
    let weight = |field: u8| weights.get(field as usize).copied().unwrap_or(1);
    for (slot, word) in q.words.iter().enumerate() {
        for m in match_terms(k, word, true, slot + 1 == slots) {
            for p in &k.terms[m.term as usize].postings {
                note(p.doc, slot, SlotHit { kind: m.kind as u8, edits: m.edits, weight: weight(p.field) });
            }
        }
        // One query word written as two adjacent words in the text ("acdc" for "AC/DC").
        let chars: Vec<(usize, char)> = word.char_indices().collect();
        if chars.len() >= 4 && !is_cjk_word(word) {
            for &(at, _) in &chars[2..=chars.len() - 2] {
                let (head, tail) = word.split_at(at);
                let Some(&head_id) = k.vocab.get(head) else { continue };
                let starts: HashSet<(u32, u8, u8)> = k.terms[head_id as usize].postings.iter().map(|p| (p.doc, p.field, p.pos)).collect();
                for (text, id) in k.vocab.range::<str, _>((std::ops::Bound::Included(tail), std::ops::Bound::Unbounded)) {
                    if !text.starts_with(tail) {
                        break;
                    }
                    let kind = if &**text == tail { Match::Exact } else { Match::Prefix };
                    for p in &k.terms[*id as usize].postings {
                        if p.pos > 0 && starts.contains(&(p.doc, p.field, p.pos - 1)) {
                            note(p.doc, slot, SlotHit { kind: kind as u8, edits: 0, weight: weight(p.field) });
                        }
                    }
                }
            }
        }
    }
    // Two adjacent query words written as one word in the text ("fleet wood"), exact only. With
    // typos, a joined pair would match either of its own words.
    for slot in 0..slots.saturating_sub(1) {
        let (a, b) = (&q.words[slot], &q.words[slot + 1]);
        if is_cjk_word(a) || is_cjk_word(b) {
            continue;
        }
        let pair = format!("{a}{b}");
        for m in match_terms(k, &pair, false, false) {
            for p in &k.terms[m.term as usize].postings {
                let hit = SlotHit { kind: m.kind as u8, edits: m.edits, weight: weight(p.field) };
                note(p.doc, slot, hit);
                note(p.doc, slot + 1, hit);
            }
        }
    }
    for (doc, _) in found.iter() {
        slot_of[*doc as usize] = 0;
    }
    let need = found.iter().map(|(_, h)| h[..slots].iter().filter(|s| s.kind > 0).count()).max().unwrap_or(0);
    if need == 0 {
        return Vec::new();
    }
    let mut ranked: Vec<Hit> = Vec::new();
    for (doc, slot_hits) in found.iter() {
        let (doc, slot_hits) = (*doc, &slot_hits[..slots]);
        let matched = slot_hits.iter().filter(|s| s.kind > 0).count();
        if matched < need {
            continue;
        }
        let d = &k.docs[doc as usize];
        if !d.alive || !allowed(d) {
            continue;
        }
        if !q.cjk_runs.is_empty() {
            let flat: String = d.text.chars().filter(|c| *c != ' ').collect();
            if !q.cjk_runs.iter().all(|run| flat.contains(run.as_str())) {
                continue;
            }
        }
        let mut whole: Option<(u8, u16)> = None;
        if matched == slots {
            for (field, &w) in d.text.split('\n').zip(weights) {
                let tier = match compare_spaceless(field, &q.joined) {
                    Some(true) => TIER_EXACT,
                    Some(false) => TIER_PREFIX,
                    None => continue,
                };
                if whole.is_none_or(|(t, bw)| (tier, std::cmp::Reverse(w)) < (t, std::cmp::Reverse(bw))) {
                    whole = Some((tier, w));
                }
            }
        }
        let worst = slot_hits.iter().filter(|s| s.kind > 0).map(|s| s.kind).min().unwrap_or(0);
        let edits: u8 = slot_hits.iter().map(|s| s.edits).sum();
        let (tier, field) = match whole {
            Some((tier, w)) => (tier, w),
            None => {
                let tier = match worst {
                    k if k >= Match::Prefix as u8 => TIER_WORDS,
                    k if k == Match::Fuzzy as u8 => TIER_FUZZY,
                    _ => TIER_INFIX,
                };
                (tier, slot_hits.iter().map(|s| s.weight).sum())
            }
        };
        let exact_words = slot_hits.iter().filter(|s| s.kind == Match::Exact as u8).count() as u8;
        let key = RankKey {
            missed: (slots - matched) as u8,
            tier,
            edits: if tier <= TIER_PREFIX { 0 } else { edits },
            field: std::cmp::Reverse(field),
            exact_words: std::cmp::Reverse(exact_words),
            popularity: std::cmp::Reverse(popularity(d)),
            length: d.name_len,
            recency: std::cmp::Reverse(d.recency),
            rowid: d.rowid,
        };
        ranked.push(Hit { rowid: d.rowid, key });
    }
    if ranked.len() > limit {
        ranked.select_nth_unstable_by_key(limit, |h| h.key);
        ranked.truncate(limit);
    }
    ranked.sort_unstable_by_key(|h| h.key);
    ranked
}

/// `Some(true)` when `field` equals `query` ignoring spaces, `Some(false)` when it starts with
/// it, else `None`. `query` has no spaces.
fn compare_spaceless(field: &str, query: &str) -> Option<bool> {
    let mut f = field.bytes().filter(|b| *b != b' ' && *b != b'\n');
    for qb in query.bytes() {
        if f.next()? != qb {
            return None;
        }
    }
    Some(f.next().is_none())
}

/// Loads the rows of ranked hits, in order.
pub(crate) fn load<T: serde::de::DeserializeOwned>(c: &Connection, kind: SearchKind, hits: &[Hit]) -> Result<Vec<T>> {
    let sql = format!("SELECT data FROM {} WHERE rowid = ?1", kind.table());
    let mut st = c.prepare_cached(&sql)?;
    let mut out = Vec::with_capacity(hits.len());
    for h in hits {
        let data: Option<String> = rusqlite::OptionalExtension::optional(st.query_row([h.rowid], |r| r.get(0)))?;
        if let Some(data) = data {
            out.push(serde_json::from_str(&data).map_err(|e| Error::Sync(e.to_string()))?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dist(a: &str, b: &str, k: u8) -> (Option<u8>, Option<u8>) {
        let mut rows: [Vec<u8>; 3] = Default::default();
        osa(a.as_bytes(), b.as_bytes(), k, &mut rows)
    }

    #[test]
    fn edit_distance_counts_transpositions_and_word_starts() {
        assert_eq!(dist("rythm", "rhythm", 1).0, Some(1));
        assert_eq!(dist("weeknd", "weekend", 1).0, Some(1));
        assert_eq!(dist("teh", "the", 1).0, Some(1), "a swap is one edit");
        assert_eq!(dist("fleetwod", "fleetwood", 2).0, Some(1));
        assert_eq!(dist("beyons", "beyonce", 1), (None, Some(1)), "a typo while typing");
        assert_eq!(dist("abcd", "wxyz", 1), (None, None));
    }

    #[test]
    fn spaceless_comparison() {
        assert_eq!(compare_spaceless("fleetwood mac", "fleetwoodmac"), Some(true));
        assert_eq!(compare_spaceless("fleetwood mac", "fleetwoodm"), Some(false));
        assert_eq!(compare_spaceless("fleetwood", "fleetwoodmac"), None);
    }
}

/// Search through a real [`super::Library`]: matching, ranking and keeping up with writes.
#[cfg(test)]
mod library_tests {
    use crate::client::{Album, Artist, MusicFolder, Song};
    use crate::library::{Library, LibraryConfig, SearchKind, StarTable, sync};

    fn library() -> (tempfile::TempDir, Library, sync::SyncCursor) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(&LibraryConfig { path: dir.path().join("lib.sqlite") }).unwrap();
        lib.replace_libraries(&[MusicFolder { id: "1".into(), name: "L".into() }]).unwrap();
        let cur = lib.begin_sync("1", None, false).unwrap();
        (dir, lib, cur)
    }

    fn song(id: &str, title: &str, artist: &str, album: &str) -> Song {
        Song { id: id.into(), title: title.into(), artist: Some(artist.into()), display_artist: Some(artist.into()), album: Some(album.into()), album_id: Some(format!("al-{album}")), ..Default::default() }
    }

    fn artist(id: &str, name: &str, albums: i64) -> Artist {
        Artist { id: id.into(), name: name.into(), album_count: Some(albums), ..Default::default() }
    }

    fn album(id: &str, name: &str, artist: &str) -> Album {
        Album { id: id.into(), name: name.into(), artist: Some(artist.into()), display_artist: Some(artist.into()), ..Default::default() }
    }

    fn titles(lib: &Library, q: &str) -> Vec<String> {
        lib.search(q, 20, None).unwrap().songs.into_iter().map(|s| s.title).collect()
    }

    fn artists(lib: &Library, q: &str) -> Vec<String> {
        lib.search(q, 20, None).unwrap().artists.into_iter().map(|a| a.name).collect()
    }

    /// A small library with near-miss names, so typos have something to confuse.
    fn sample() -> (tempfile::TempDir, Library) {
        let (d, lib, cur) = library();
        lib.apply_artists("1", &cur, 0, &[
            artist("weeknd", "The Weeknd", 5), artist("fm", "Fleetwood Mac", 3), artist("bey", "Beyoncé", 4),
            artist("kl", "Kendrick Lamar", 4), artist("acdc", "AC/DC", 2), artist("drake", "Drake", 6),
            artist("drakebell", "Drake Bell", 1), artist("mp", "The Mamas & The Papas", 2), artist("am", "Arctic Monkeys", 3),
        ]).unwrap();
        lib.apply_albums("1", &cur, 0, &[album("rumours", "Rumours", "Fleetwood Mac"), album("lemonade", "Lemonade", "Beyoncé")]).unwrap();
        lib.apply_songs("1", &cur, 0, &[
            song("weekend", "Weekend", "Someone Else", "Days"),
            song("blinding", "Blinding Lights", "The Weeknd", "After Hours"),
            song("landslide", "Landslide", "Fleetwood Mac", "Fleetwood Mac"),
            song("dreams", "Dreams", "Fleetwood Mac", "Rumours"),
            song("rhythm", "I Got Rhythm", "Ella Fitzgerald", "Songbook"),
            song("halo", "Halo", "Beyoncé", "I Am... Sasha Fierce"),
            song("humble", "HUMBLE.", "Kendrick Lamar", "DAMN."),
            song("thunder", "Thunderstruck", "AC/DC", "The Razors Edge"),
            song("yours", "I Wanna Be Yours", "Arctic Monkeys", "AM"),
            song("sweet", "Sweet Dreams", "Eurythmics", "Sweet Dreams"),
            song("mondays", "Monday Monday", "The Mamas & The Papas", "If You Can Believe Your Eyes"),
            song("hotline", "Hotline Bling", "Drake", "Views"),
            song("drakefreestyle", "Drake Freestyle", "Someone", "Mixtape"),
            song("cjk", "堕天使", "King Gnu", "CEREMONY"),
            song("kr", "봄날", "방탄소년단", "You Never Walk Alone"),
            song("fw", "ＦＵＬＬ ＷＩＤＴＨ", "Wide", "Ｗ"),
        ]).unwrap();
        (d, lib)
    }

    #[test]
    fn typos_find_what_was_meant() {
        let (_d, lib) = sample();
        assert_eq!(artists(&lib, "fleetwod mac")[0], "Fleetwood Mac");
        assert_eq!(artists(&lib, "beyonse")[0], "Beyoncé");
        assert_eq!(artists(&lib, "kendrik lamr")[0], "Kendrick Lamar");
        assert_eq!(titles(&lib, "rythm")[0], "I Got Rhythm", "a missing letter");
        assert_eq!(titles(&lib, "humbel")[0], "HUMBLE.", "swapped letters");
        assert_eq!(titles(&lib, "lansdlide fleetwood")[0], "Landslide");
        assert_eq!(artists(&lib, "beyons")[0], "Beyoncé", "a typo while still typing");
        assert!(titles(&lib, "qqqqqq").is_empty());
    }

    #[test]
    fn short_words_need_to_be_right() {
        let (_d, lib) = sample();
        // Three letters are too short to forgive a typo; four forgive one.
        assert!(titles(&lib, "hal").contains(&"Halo".to_string()));
        assert!(!titles(&lib, "hsl").contains(&"Halo".to_string()));
        assert!(titles(&lib, "hslo").contains(&"Halo".to_string()));
    }

    #[test]
    fn joined_split_and_partial_words() {
        let (_d, lib) = sample();
        assert_eq!(artists(&lib, "fleetwoodmac")[0], "Fleetwood Mac");
        assert_eq!(artists(&lib, "fleetwoodm")[0], "Fleetwood Mac", "joined while typing");
        assert_eq!(titles(&lib, "thunder struck")[0], "Thunderstruck");
        assert_eq!(titles(&lib, "wanna be urs")[0], "I Wanna Be Yours", "a word's end");
        assert_eq!(titles(&lib, "hotlinebling")[0], "Hotline Bling");
    }

    #[test]
    fn accents_case_symbols_and_full_width() {
        let (_d, lib) = sample();
        assert_eq!(artists(&lib, "BEYONCE")[0], "Beyoncé");
        assert_eq!(artists(&lib, "acdc")[0], "AC/DC");
        assert_eq!(artists(&lib, "ac dc")[0], "AC/DC");
        assert_eq!(artists(&lib, "ac/dc")[0], "AC/DC");
        assert_eq!(artists(&lib, "mamas and the papas")[0], "The Mamas & The Papas");
        assert_eq!(artists(&lib, "mamas & papas")[0], "The Mamas & The Papas");
        assert_eq!(titles(&lib, "humble")[0], "HUMBLE.");
        assert_eq!(titles(&lib, "full width")[0], "ＦＵＬＬ ＷＩＤＴＨ");
        assert_eq!(titles(&lib, "ｆｕｌｌ")[0], "ＦＵＬＬ ＷＩＤＴＨ");
    }

    #[test]
    fn cjk_and_korean_match_inside_words() {
        let (_d, lib) = sample();
        assert_eq!(titles(&lib, "天使"), ["堕天使"]);
        assert!(titles(&lib, "使天").is_empty(), "CJK characters must appear in order");
        assert_eq!(titles(&lib, "봄"), ["봄날"]);
        assert_eq!(titles(&lib, "소년"), ["봄날"], "Hangul inside a word");
        assert_eq!(titles(&lib, "king gnu"), ["堕天使"]);
    }

    #[test]
    fn words_match_across_fields() {
        let (_d, lib) = sample();
        assert_eq!(titles(&lib, "landslide fleetwood"), ["Landslide"]);
        assert_eq!(titles(&lib, "fleetwood dreams"), ["Dreams"]);
        assert_eq!(titles(&lib, "dreams rumours"), ["Dreams"]);
    }

    /// Exact, prefix and word-start matches always come before fuzzy ones.
    #[test]
    fn exact_matches_always_rank_above_fuzzy_ones() {
        let (_d, lib) = sample();
        let r = lib.search("weekend", 20, None).unwrap();
        assert_eq!(r.songs[0].title, "Weekend", "the song really called Weekend first");
        assert_eq!(r.songs[1].title, "Blinding Lights", "The Weeknd's songs only after it");
        assert_eq!(r.artists[0].name, "The Weeknd", "still found, as a typo");
        assert_eq!(r.best, Some(SearchKind::Song), "the song leads, not The Weeknd");

        let r = lib.search("weeknd", 20, None).unwrap();
        assert_eq!(r.artists[0].name, "The Weeknd");
        assert_eq!(r.songs[0].title, "Blinding Lights");
        assert_eq!(r.songs.last().unwrap().title, "Weekend", "the typo match comes last");
        assert_eq!(r.best, Some(SearchKind::Artist));

        // Exact titles, then one that starts with the query, then a word inside one.
        let t = titles(&lib, "dreams");
        assert_eq!(t[0], "Dreams");
        assert_eq!(t[1], "Sweet Dreams");
    }

    #[test]
    fn exact_then_prefix_then_words_then_typos() {
        let (_d, lib, cur) = library();
        lib.apply_songs("1", &cur, 0, &[
            song("fuzzy", "Togehter", "X", "Y"),
            song("words", "Come Together", "X", "Y"),
            song("prefix", "Together Again", "X", "Y"),
            song("exact", "Together", "X", "Y"),
            song("inside", "Altogether", "X", "Y"),
        ]).unwrap();
        // Plays and stars never lift a weaker match over a stronger one.
        lib.set_starred(StarTable::Song, "fuzzy", true).unwrap();
        for _ in 0..50 {
            lib.record_play("inside", 1).unwrap();
        }
        assert_eq!(titles(&lib, "together"), ["Together", "Together Again", "Come Together", "Togehter", "Altogether"]);
    }

    #[test]
    fn the_artist_leads_and_popularity_breaks_ties() {
        let (_d, lib) = sample();
        let r = lib.search("drake", 20, None).unwrap();
        assert_eq!(r.artists[0].name, "Drake");
        assert_eq!(r.best, Some(SearchKind::Artist));
        assert_eq!(r.songs[0].title, "Hotline Bling", "Drake's own song: its artist field is exactly the query");
        // Two exact titles: the starred and played one first.
        let (_d2, lib, cur) = library();
        lib.apply_songs("1", &cur, 0, &[song("a", "Intro", "A", "One"), song("b", "Intro", "B", "Two"), song("c", "Intro", "C", "Three")]).unwrap();
        lib.record_play("c", 1).unwrap();
        lib.set_starred(StarTable::Song, "b", true).unwrap();
        let ids: Vec<String> = lib.search("intro", 10, None).unwrap().songs.into_iter().map(|s| s.id).collect();
        assert_eq!(ids, ["b", "c", "a"]);
    }

    #[test]
    fn index_follows_renames_removals_and_new_rows() {
        let (_d, lib, cur) = library();
        lib.apply_songs("1", &cur, 0, &[song("a", "Old Name", "X", "Y")]).unwrap();
        assert_eq!(titles(&lib, "old"), ["Old Name"]);
        lib.apply_songs("1", &cur, 0, &[song("a", "New Name", "X", "Y"), song("b", "Brand New", "X", "Y")]).unwrap();
        assert!(titles(&lib, "old").is_empty());
        assert_eq!(titles(&lib, "new"), ["New Name", "Brand New"]);
        // A later generation that doesn't see "b" removes it.
        lib.finish_library("1", &cur, None).unwrap();
        let next = lib.begin_sync("1", None, false).unwrap();
        lib.apply_songs("1", &next, 0, &[song("a", "New Name", "X", "Y")]).unwrap();
        lib.finish_library("1", &next, None).unwrap();
        assert_eq!(titles(&lib, "new"), ["New Name"]);
    }

    #[test]
    fn a_failed_write_changes_nothing() {
        let (_d, lib, cur) = library();
        lib.apply_songs("1", &cur, 0, &[song("a", "Kept", "X", "Y")]).unwrap();
        assert_eq!(titles(&lib, "kept"), ["Kept"]);
        let failed: crate::error::Result<()> = lib.write(|tx| {
            tx.execute("UPDATE song SET content_hash = 'x', data = json_set(data, '$.title', 'Lost') WHERE id = 'a'", [])?;
            Err(crate::error::Error::Sync("abandon".into()))
        });
        assert!(failed.is_err());
        assert_eq!(titles(&lib, "kept"), ["Kept"]);
    }

    #[test]
    fn library_filter_applies_to_every_kind() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(&LibraryConfig { path: dir.path().join("lib.sqlite") }).unwrap();
        lib.replace_libraries(&[MusicFolder { id: "1".into(), name: "A".into() }, MusicFolder { id: "2".into(), name: "B".into() }]).unwrap();
        let c1 = lib.begin_sync("1", None, false).unwrap();
        let c2 = lib.begin_sync("2", None, false).unwrap();
        lib.apply_artists("1", &c1, 0, &[artist("x", "Shared Artist", 1)]).unwrap();
        lib.apply_songs("1", &c1, 0, &[song("a", "Shared", "X", "Y")]).unwrap();
        lib.apply_songs("2", &c2, 0, &[song("b", "Shared", "X", "Y")]).unwrap();
        let only2 = lib.search("shared", 10, Some(&["2".to_string()])).unwrap();
        assert_eq!(only2.songs.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["b"]);
        assert!(only2.artists.is_empty());
        assert_eq!(lib.search("shared", 10, Some(&["1".to_string()])).unwrap().artists.len(), 1);
    }
}
