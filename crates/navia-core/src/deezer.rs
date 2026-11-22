//! Deezer's public API, and the one [`gate`] every Deezer use goes through.
//! Deezer is used only when the listener turns on [`setting`]. Only names, ids, lengths and ISRCs are kept.
//! Requests carry no user or server details, are paced and cached, and back off on the quota.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::time::Instant;

use crate::client::user_agent;
use crate::lanyard::matching::{find_named_or_bare, first_credited, key};
use crate::library::Library;

/// The account setting's key in `meta`. "1" is on, "0" off and absent unset.
const SETTING_KEY: &str = "deezer_recommendations";

/// The listener's Deezer choice for this account, or `None` until they choose.
pub fn setting(db: &Library) -> Option<bool> {
    match db.meta(SETTING_KEY).ok().flatten().as_deref() {
        Some("1") => Some(true),
        Some("0") => Some(false),
        _ => None,
    }
}

/// Saves the listener's choice. `None` forgets it, so the apps ask again.
pub fn set_setting(db: &Library, on: Option<bool>) -> crate::Result<()> {
    db.set_meta(SETTING_KEY, on.map(|on| if on { "1" } else { "0" }))
}

/// Whether Deezer may be used now, and if not, why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// The listener turned it on.
    Open,
    /// The listener hasn't chosen yet, which counts as off.
    NotChosen,
    /// The listener turned it off.
    Off,
}

impl Gate {
    pub fn is_open(self) -> bool {
        self == Gate::Open
    }
}

/// The gate every Deezer use goes through.
pub fn gate(db: &Library) -> Gate {
    match setting(db) {
        None => Gate::NotChosen,
        Some(false) => Gate::Off,
        Some(true) => Gate::Open,
    }
}

/// Deezer's public API.
pub const DEEZER_API: &str = "https://api.deezer.com";

/// Tracks asked for per radio call. Deezer caps it at 100.
const RADIO_LIMIT: u32 = 100;
/// Time Deezer gets per radio, from the artist search to the last ISRC. Past it, the server fills the rest.
const BUDGET: Duration = Duration::from_secs(5);
/// Timeout for one request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Deezer's largest page.
const PAGE_LIMIT: usize = 100;
/// pages read of one album's tracks.
const ALBUM_PAGES: usize = 5;
const ALBUM_SEARCH_LIMIT: u32 = 10;
/// time between two requests: 40 in 5 s. Deezer refuses the 51st request inside 5 s (measured); the
/// room left is for other software on the same address.
const PACE: Duration = Duration::from_millis(125);
/// Deezer artists with the same name compared by their top tracks, at most.
const SAME_NAME_CHECKS: usize = 3;
/// Cache lifetimes for a name's artist, a name Deezer lacks, a radio's tracks and a track's ISRC.
const ARTIST_TTL: Duration = Duration::from_secs(6 * 3600);
const ARTIST_MISS_TTL: Duration = Duration::from_secs(3600);
const RADIO_TTL: Duration = Duration::from_secs(15 * 60);
const ISRC_TTL: Duration = Duration::from_secs(24 * 3600);
/// Entries per cache before expired ones are dropped, then all if it's still full.
const CACHE_MAX: usize = 2000;
/// Wait after Deezer reports the quota used up. Doubles while it keeps saying so.
const BACKOFF_FIRST: Duration = Duration::from_secs(10);
const BACKOFF_MAX: Duration = Duration::from_secs(10 * 60);
/// Deezer's error code for the quota.
const QUOTA_CODE: i64 = 4;
/// requests per hour at most, for everything Navia asks Deezer. past it, cached answers serve until the hour moves on.
const HOURLY_CAP: usize = 3000;
/// What the answers kept in the library may take (`deezer_cache`), least recently used out first.
const STORE_MAX_BYTES: i64 = 20 * 1024 * 1024;


/// Why Deezer gave nothing usable.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DeezerError {
    #[error("Deezer's quota is used up")]
    Quota,
    #[error("waiting after Deezer's quota until it passes")]
    BackingOff,
    #[error("Deezer took too long")]
    Timeout,
    #[error("couldn't reach Deezer: {0}")]
    Connection(String),
    #[error("Deezer answered HTTP {0}")]
    Http(u16),
    #[error("Navia's hourly allowance of Deezer requests is used up")]
    HourlyCap,
    #[error("Deezer error {code}: {message}")]
    Api { code: i64, message: String },
    #[error("unreadable answer from Deezer: {0}")]
    Parse(String),
}

/// One track of a Deezer radio, with only what matching needs.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct DeezerTrack {
    pub id: u64,
    pub title: String,
    /// Seconds.
    pub duration: Option<u64>,
    pub artist: Named,
    pub album: Titled,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Named {
    pub id: u64,
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Titled {
    pub title: String,
}

/// one track of a Deezer album, with its popularity.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct AlbumTrack {
    pub id: u64,
    pub title: String,
    pub title_short: String,
    /// seconds.
    pub duration: Option<u64>,
    pub rank: u64,
    pub isrc: Option<String>,
    pub track_position: Option<u32>,
    pub disk_number: Option<u32>,
}

/// a Deezer album from a search.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct FoundAlbum {
    pub id: u64,
    pub title: String,
    pub artist: Named,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct DeezerArtist {
    id: u64,
    name: String,
    nb_fan: u64,
}

#[derive(Default)]
struct Caches {
    /// Name key → Deezer artist id, `None` when Deezer has no artist by that name.
    artists: HashMap<String, (Instant, Option<u64>)>,
    /// Deezer artist id → the radio's tracks so far.
    radios: HashMap<u64, (Instant, Vec<DeezerTrack>)>,
    /// Deezer track id → its ISRC, `None` when it has none.
    isrcs: HashMap<u64, (Instant, Option<String>)>,
    /// No requests before this, after hitting the quota.
    blocked_until: Option<Instant>,
    backoff: Option<Duration>,
    /// When each request of the last hour went.
    recent: VecDeque<Instant>,
}

fn trim<K: Clone + Eq + std::hash::Hash, V>(map: &mut HashMap<K, (Instant, V)>, ttl: Duration) {
    if map.len() >= CACHE_MAX {
        let now = Instant::now();
        map.retain(|_, (at, _)| now.duration_since(*at) < ttl);
        if map.len() >= CACHE_MAX {
            map.clear();
        }
    }
}

fn fresh<T: Clone>(entry: Option<&(Instant, T)>, ttl: Duration) -> Option<T> {
    entry.filter(|(at, _)| at.elapsed() < ttl).map(|(_, v)| v.clone())
}

/// Deezer's public API with caches, pacing and quota backoff. One per process, see [`Deezer::shared`].
pub struct Deezer {
    base: String,
    http: OnceLock<reqwest::Client>,
    caches: Mutex<Caches>,
    /// When the next request may go.
    next_slot: tokio::sync::Mutex<Instant>,
    pace: Duration,
    pub(crate) budget: Duration,
    hourly: usize,
    /// Requests in flight by URL, so a repeated request waits for the one under way.
    inflight: Mutex<HashMap<String, Arc<tokio::sync::OnceCell<Result<Value, DeezerError>>>>>,
}

type Answer = Result<Value, DeezerError>;

impl Deezer {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            http: OnceLock::new(),
            caches: Mutex::default(),
            next_slot: tokio::sync::Mutex::new(Instant::now()),
            pace: PACE,
            budget: BUDGET,
            hourly: HOURLY_CAP,
            inflight: Mutex::default(),
        }
    }

    /// The instance the apps use, against [`DEEZER_API`].
    pub fn shared() -> &'static Deezer {
        static SHARED: OnceLock<Deezer> = OnceLock::new();
        SHARED.get_or_init(|| Deezer::new(DEEZER_API))
    }

    /// Test hook for another pace and time budget per radio.
    pub fn with_timing(mut self, pace: Duration, budget: Duration) -> Self {
        self.pace = pace;
        self.budget = budget;
        self
    }

    /// Test hook for another hourly cap.
    pub fn with_hourly_cap(mut self, cap: usize) -> Self {
        self.hourly = cap;
        self
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        // Built at first use, so the app's version is set by then (`client::set_app_version`).
        self.http.get_or_init(|| reqwest::Client::builder().user_agent(user_agent()).connect_timeout(CONNECT_TIMEOUT).build().unwrap_or_default())
    }

    /// One GET, paced, within `deadline`, with Deezer's errors read out of its 200 answers. The
    /// same request asked again while one is under way waits for that one's answer.
    pub(crate) async fn get(&self, path: &str, query: &[(&str, String)], deadline: Instant) -> Answer {
        let mut url = url::Url::parse(&format!("{}{path}", self.base)).map_err(|e| DeezerError::Connection(e.to_string()))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
        }
        let name = url.to_string();
        let cell = self.inflight.lock().unwrap().entry(name.clone()).or_default().clone();
        let answer = cell.get_or_init(|| self.send(url, deadline)).await.clone();
        let mut inflight = self.inflight.lock().unwrap();
        if inflight.get(&name).is_some_and(|c| Arc::ptr_eq(c, &cell)) {
            inflight.remove(&name);
        }
        answer
    }

    async fn send(&self, url: url::Url, deadline: Instant) -> Answer {
        {
            let mut caches = self.caches.lock().unwrap();
            if caches.blocked_until.is_some_and(|until| Instant::now() < until) {
                return Err(DeezerError::BackingOff);
            }
            let now = Instant::now();
            while caches.recent.front().is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(3600)) {
                caches.recent.pop_front();
            }
            if caches.recent.len() >= self.hourly {
                return Err(DeezerError::HourlyCap);
            }
            caches.recent.push_back(now);
        }
        {
            let mut slot = self.next_slot.lock().await;
            let at = (*slot).max(Instant::now());
            if at >= deadline {
                return Err(DeezerError::Timeout);
            }
            *slot = at + self.pace;
            drop(slot);
            tokio::time::sleep_until(at).await;
        }
        let left = deadline.saturating_duration_since(Instant::now()).min(REQUEST_TIMEOUT);
        if left.is_zero() {
            return Err(DeezerError::Timeout);
        }
        let send = async {
            let response = self.http().get(url).send().await.map_err(|e| DeezerError::Connection(e.without_url().to_string()))?;
            let status = response.status();
            if status.as_u16() == 429 {
                return Err(DeezerError::Quota);
            }
            if !status.is_success() {
                return Err(DeezerError::Http(status.as_u16()));
            }
            response.json::<Value>().await.map_err(|e| DeezerError::Parse(e.without_url().to_string()))
        };
        let result = match tokio::time::timeout(left, send).await {
            Err(_) => Err(DeezerError::Timeout),
            Ok(Err(e)) => Err(e),
            Ok(Ok(body)) => match body.get("error") {
                Some(error) => {
                    let code = error.get("code").and_then(Value::as_i64).unwrap_or_default();
                    let message = error.get("message").and_then(Value::as_str).unwrap_or_default().to_string();
                    let quota = code == QUOTA_CODE || error.get("type").and_then(Value::as_str) == Some("QUOTA") || message.to_ascii_lowercase().contains("quota");
                    Err(if quota { DeezerError::Quota } else { DeezerError::Api { code, message } })
                }
                None => Ok(body),
            },
        };
        let mut caches = self.caches.lock().unwrap();
        match &result {
            Err(DeezerError::Quota) => {
                let wait = caches.backoff.map_or(BACKOFF_FIRST, |b| (b * 2).min(BACKOFF_MAX));
                caches.backoff = Some(wait);
                caches.blocked_until = Some(Instant::now() + wait);
                tracing::info!(?wait, "Deezer quota reached; Artist Radio uses the server's radio meanwhile");
            }
            Ok(_) => {
                caches.backoff = None;
                caches.blocked_until = None;
            }
            Err(_) => {}
        }
        result
    }

    fn data<T: for<'de> Deserialize<'de>>(body: Value) -> Result<Vec<T>, DeezerError> {
        let items = body.get("data").cloned().unwrap_or(Value::Array(Vec::new()));
        let items = items.as_array().cloned().unwrap_or_default();
        Ok(items.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
    }

    /// The Deezer artist for a library artist, by exact normalized name.
    /// Among namesakes, the one whose top tracks the library has under this artist wins, then the most fans.
    pub(crate) async fn artist_id(&self, db: &Library, name: &str, deadline: Instant) -> Result<Option<u64>, DeezerError> {
        let wanted = key(name);
        if wanted.is_empty() {
            return Ok(None);
        }
        if let Some(entry) = self.caches.lock().unwrap().artists.get(&wanted) {
            let ttl = if entry.1.is_some() { ARTIST_TTL } else { ARTIST_MISS_TTL };
            if let Some(known) = fresh(Some(entry), ttl) {
                return Ok(known);
            }
        }
        // the name as given first; only when Deezer has no artist by it, the first credited one.
        let mut same: Vec<DeezerArtist> = Vec::new();
        let first = first_credited(name);
        for asked in std::iter::once(name).chain((first != name.trim()).then_some(first)) {
            let body = self.get("/search/artist", &[("q", asked.to_string()), ("limit", "10".to_string())], deadline).await?;
            let exact = key(asked);
            same = Self::data::<DeezerArtist>(body)?.into_iter().filter(|a| a.id > 0 && !exact.is_empty() && key(&a.name) == exact).collect();
            if !same.is_empty() {
                break;
            }
        }
        same.sort_by_key(|a| std::cmp::Reverse(a.nb_fan));
        let found = match same.len() {
            0 => None,
            1 => Some(same[0].id),
            _ => {
                let mut best: Option<(usize, u64)> = None;
                for artist in same.iter().take(SAME_NAME_CHECKS) {
                    let Ok(body) = self.get(&format!("/artist/{}/top", artist.id), &[("limit", "10".to_string())], deadline).await else { break };
                    let tracks = Self::data::<DeezerTrack>(body).unwrap_or_default();
                    let owned = tracks.iter().filter(|t| find_named_or_bare(db, &t.title, &[name], &t.album.title, t.duration.map(|d| d * 1000)).is_some()).count();
                    if best.is_none_or(|(n, _)| owned > n) {
                        best = Some((owned, artist.id));
                    }
                }
                best.map(|(_, id)| id).or(Some(same[0].id))
            }
        };
        let mut caches = self.caches.lock().unwrap();
        trim(&mut caches.artists, ARTIST_TTL);
        caches.artists.insert(wanted, (Instant::now(), found));
        Ok(found)
    }

    /// Calls the artist's radio once more and returns every track seen so far.
    pub(crate) async fn radio_call(&self, artist: u64, deadline: Instant) -> Result<Vec<DeezerTrack>, DeezerError> {
        let body = self.get(&format!("/artist/{artist}/radio"), &[("limit", RADIO_LIMIT.to_string())], deadline).await?;
        let new = Self::data::<DeezerTrack>(body)?;
        let mut caches = self.caches.lock().unwrap();
        trim(&mut caches.radios, RADIO_TTL);
        let entry = caches.radios.entry(artist).or_insert_with(|| (Instant::now(), Vec::new()));
        let mut seen: HashSet<u64> = entry.1.iter().map(|t| t.id).collect();
        entry.1.extend(new.into_iter().filter(|t| t.id > 0 && seen.insert(t.id)));
        Ok(entry.1.clone())
    }

    pub(crate) fn cached_radio(&self, artist: u64) -> Option<Vec<DeezerTrack>> {
        fresh(self.caches.lock().unwrap().radios.get(&artist), RADIO_TTL)
    }

    /// One fresh, uncached draw of an artist's radio, up to 100 tracks.
    pub(crate) async fn radio_draw(&self, artist: u64, deadline: Instant) -> Result<Vec<DeezerTrack>, DeezerError> {
        let body = self.get(&format!("/artist/{artist}/radio"), &[("limit", RADIO_LIMIT.to_string())], deadline).await?;
        Ok(Self::data::<DeezerTrack>(body)?.into_iter().filter(|t| t.id > 0).collect())
    }

    /// The names of the artists Deezer relates to this one, best first.
    pub(crate) async fn related_names(&self, artist: u64, deadline: Instant) -> Result<Vec<String>, DeezerError> {
        let body = self.get(&format!("/artist/{artist}/related"), &[("limit", "20".to_string())], deadline).await?;
        Ok(Self::data::<DeezerArtist>(body)?.into_iter().map(|a| a.name).filter(|n| !n.trim().is_empty()).collect())
    }

    /// An artist's most popular tracks, most popular first.
    pub(crate) async fn top_tracks(&self, artist: u64, limit: u32, deadline: Instant) -> Result<Vec<DeezerTrack>, DeezerError> {
        let body = self.get(&format!("/artist/{artist}/top"), &[("limit", limit.to_string())], deadline).await?;
        Ok(Self::data::<DeezerTrack>(body)?.into_iter().filter(|t| t.id > 0).collect())
    }

    /// The track with this ISRC, as Deezer gives it.
    pub(crate) async fn track_by_isrc(&self, isrc: &str, deadline: Instant) -> Result<Value, DeezerError> {
        self.get(&format!("/track/isrc:{isrc}"), &[], deadline).await
    }

    /// a Deezer album's tracks with their `rank`, in album order.
    pub(crate) async fn album_tracks(&self, album: u64, deadline: Instant) -> Result<Vec<AlbumTrack>, DeezerError> {
        let mut tracks = Vec::new();
        for page in 0..ALBUM_PAGES {
            let query = [("index", (page * PAGE_LIMIT).to_string()), ("limit", PAGE_LIMIT.to_string())];
            let body = self.get(&format!("/album/{album}/tracks"), &query, deadline).await?;
            let more = body.get("next").and_then(Value::as_str).is_some();
            let found = Self::data::<AlbumTrack>(body)?;
            let full = found.len() >= PAGE_LIMIT;
            tracks.extend(found.into_iter().filter(|t| t.id > 0));
            if !more || !full {
                break;
            }
        }
        Ok(tracks)
    }

    /// albums Deezer finds for an artist and title (`/search/album`), best first.
    pub(crate) async fn search_album(&self, artist: &str, title: &str, deadline: Instant) -> Result<Vec<FoundAlbum>, DeezerError> {
        let q = format!("artist:\"{}\" album:\"{}\"", artist.replace('"', ""), title.replace('"', ""));
        let body = self.get("/search/album", &[("q", q), ("limit", ALBUM_SEARCH_LIMIT.to_string())], deadline).await?;
        Ok(Self::data::<FoundAlbum>(body)?.into_iter().filter(|a| a.id > 0).collect())
    }

    /// A track's ISRC, on its own (for checks against the real service).
    pub async fn track_isrc(&self, track: u64) -> Option<String> {
        self.isrc(track, Instant::now() + REQUEST_TIMEOUT).await
    }

    /// A track's ISRC (`/track/{id}`), cached.
    pub(crate) async fn isrc(&self, track: u64, deadline: Instant) -> Option<String> {
        if let Some(known) = fresh(self.caches.lock().unwrap().isrcs.get(&track), ISRC_TTL) {
            return known;
        }
        let body = self.get(&format!("/track/{track}"), &[], deadline).await.ok()?;
        let isrc = body.get("isrc").and_then(Value::as_str).map(|i| i.trim().to_ascii_uppercase()).filter(|i| !i.is_empty());
        let mut caches = self.caches.lock().unwrap();
        trim(&mut caches.isrcs, ISRC_TTL);
        caches.isrcs.insert(track, (Instant::now(), isrc.clone()));
        isrc
    }

}

/// A kept answer from `deezer_cache`, with its fetch time in ms.
pub(crate) fn stored(db: &Library, key: &str) -> Option<(i64, String)> {
    db.read(|c| Ok(c.query_row("SELECT fetched_ms, value FROM deezer_cache WHERE key = ?1", [key], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)).ok().flatten()
}

/// Keeps a Deezer answer in the library, then trims the least recently used past [`STORE_MAX_BYTES`].
/// Only names, ids, lengths, ISRCs and BPMs are kept, never images.
pub(crate) fn store(db: &Library, key: &str, value: &str, now_ms: i64) {
    let result = db.write_quiet(|tx| {
        tx.execute(
            "INSERT INTO deezer_cache(key, fetched_ms, used_ms, value) VALUES (?1, ?2, ?2, ?3) ON CONFLICT(key) DO UPDATE SET fetched_ms = excluded.fetched_ms, used_ms = excluded.used_ms, value = excluded.value",
            params![key, now_ms, value],
        )?;
        let total: i64 = tx.query_row("SELECT coalesce(sum(length(value)), 0) FROM deezer_cache", [], |r| r.get(0))?;
        if total > STORE_MAX_BYTES {
            // Trim to nine tenths so trimming doesn't run on every write once full.
            tx.execute(
                "DELETE FROM deezer_cache WHERE key IN (SELECT key FROM (SELECT key, sum(length(value)) OVER (ORDER BY used_ms DESC, key) AS upto FROM deezer_cache) WHERE upto > ?1)",
                [STORE_MAX_BYTES * 9 / 10],
            )?;
        }
        Ok(())
    });
    if let Err(e) = result {
        tracing::warn!("Deezer cache: {e}");
    }
}

/// Marks a kept answer as used now, for the least-recently-used trim.
pub(crate) fn touch(db: &Library, key: &str, now_ms: i64) {
    let _ = db.write_quiet(|tx| {
        tx.execute("UPDATE deezer_cache SET used_ms = ?2 WHERE key = ?1", params![key, now_ms])?;
        Ok(())
    });
}
