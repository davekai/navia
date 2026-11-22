//! ListenBrainz's labs API as a source for the Navia mode,
//! asked from this device only with the listener's recommendations switch on
//! ([`crate::deezer::gate`]). the server asks ListenBrainz itself only when its Last.fm agent
//! finds nothing, so with Last.fm on these answers never reach Navia otherwise.
//!
//! - a song's recording MBID: its own tag, else `acr-lookup` (artist credit and title, as given,
//!   then bare: [`crate::lanyard::matching`]).
//! - `similar-recordings` for that MBID with [`ALGORITHM`].
//!
//! anonymous (no token), Navia's User-Agent, at most one request per [`PACE`], and after a 429 no
//! request until `X-RateLimit-Reset-In` has passed. answers are kept in memory for the session.
//! it's a labs endpoint: every failure is silent and only means nothing from it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::Instant;

use crate::client::user_agent;

/// ListenBrainz's labs API.
pub const LABS_API: &str = "https://labs.api.listenbrainz.org";
/// the similar-recordings dataset asked for: sessions over 9000 days, the one the labs pages use.
pub const ALGORITHM: &str = "session_based_days_9000_session_300_contribution_5_threshold_15_limit_50_skip_30";
/// time between two requests: about 2 a second.
const PACE: Duration = Duration::from_millis(500);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// the wait after a 429 that names none.
const BACKOFF_DEFAULT: Duration = Duration::from_secs(10);
/// answers kept per kind before the oldest go.
const KEEP_MAX: usize = 2000;

/// one similar recording, only what mapping needs.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Recording {
    pub recording_mbid: String,
    pub recording_name: String,
    pub artist_credit_name: String,
    pub release_name: Option<String>,
}

#[derive(Default)]
struct State {
    blocked_until: Option<Instant>,
    /// recording MBID → its similar recordings.
    similar: Vec<(String, Vec<Recording>)>,
    /// (artist credit, title) → its recording MBID (`None`: not found).
    mbids: HashMap<(String, String), Option<String>>,
}

/// the labs API with its pacing, back-off and session memory. one per process
/// ([`ListenBrainz::shared`]); tests make their own against a fake.
pub struct ListenBrainz {
    base: String,
    http: OnceLock<reqwest::Client>,
    state: Mutex<State>,
    next_slot: tokio::sync::Mutex<Instant>,
    pace: Duration,
}

impl ListenBrainz {
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into().trim_end_matches('/').to_string(), http: OnceLock::new(), state: Mutex::default(), next_slot: tokio::sync::Mutex::new(Instant::now()), pace: PACE }
    }

    /// the one used by the apps, against [`LABS_API`].
    pub fn shared() -> &'static ListenBrainz {
        static SHARED: OnceLock<ListenBrainz> = OnceLock::new();
        SHARED.get_or_init(|| ListenBrainz::new(LABS_API))
    }

    pub fn with_pace(mut self, pace: Duration) -> Self {
        self.pace = pace;
        self
    }

    fn http(&self) -> &reqwest::Client {
        self.http.get_or_init(|| reqwest::Client::builder().user_agent(user_agent()).connect_timeout(CONNECT_TIMEOUT).build().unwrap_or_default())
    }

    /// one paced request within `deadline`; `None` on any failure.
    async fn send(&self, request: reqwest::RequestBuilder, deadline: Instant) -> Option<Value> {
        if self.state.lock().unwrap().blocked_until.is_some_and(|until| Instant::now() < until) {
            return None;
        }
        {
            let mut slot = self.next_slot.lock().await;
            let at = (*slot).max(Instant::now());
            if at >= deadline {
                return None;
            }
            *slot = at + self.pace;
            drop(slot);
            tokio::time::sleep_until(at).await;
        }
        let left = deadline.saturating_duration_since(Instant::now()).min(REQUEST_TIMEOUT);
        let response = tokio::time::timeout(left, request.send()).await.ok()?.ok()?;
        if response.status().as_u16() == 429 {
            let wait = response.headers().get("X-RateLimit-Reset-In").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok()).map_or(BACKOFF_DEFAULT, Duration::from_secs);
            self.state.lock().unwrap().blocked_until = Some(Instant::now() + wait);
            tracing::info!(?wait, "ListenBrainz rate limit reached; waiting");
            return None;
        }
        if !response.status().is_success() {
            return None;
        }
        let left = deadline.saturating_duration_since(Instant::now()).min(REQUEST_TIMEOUT);
        tokio::time::timeout(left, response.json::<Value>()).await.ok()?.ok()
    }

    /// the recording MBIDs for (artist credit, title) pairs, in one request for those not known
    /// this session. `None` for a pair it doesn't know or when it couldn't be asked; only answers
    /// it gave are kept.
    pub async fn recording_mbids(&self, wanted: &[(String, String)], deadline: Instant) -> Vec<Option<String>> {
        let ask: Vec<&(String, String)> = {
            let state = self.state.lock().unwrap();
            let mut ask: Vec<&(String, String)> = wanted.iter().filter(|w| !state.mbids.contains_key(*w)).collect();
            ask.dedup();
            ask
        };
        if !ask.is_empty() {
            let body: Vec<Value> = ask.iter().map(|(artist, title)| json!({"artist_credit_name": artist, "recording_name": title})).collect();
            let request = self.http().post(format!("{}/acr-lookup/json", self.base)).json(&body);
            if let Some(Value::Array(found)) = self.send(request, deadline).await {
                let mut by_index: HashMap<usize, String> = HashMap::new();
                for (n, item) in found.iter().enumerate() {
                    let index = item.get("index").and_then(Value::as_u64).map_or(n, |i| i as usize);
                    if let Some(mbid) = item.get("recording_mbid").and_then(Value::as_str).filter(|m| !m.is_empty()) {
                        by_index.insert(index, mbid.to_string());
                    }
                }
                let mut state = self.state.lock().unwrap();
                if state.mbids.len() >= KEEP_MAX {
                    state.mbids.clear();
                }
                for (i, pair) in ask.iter().enumerate() {
                    state.mbids.insert((*pair).clone(), by_index.get(&i).cloned());
                }
            }
        }
        let state = self.state.lock().unwrap();
        wanted.iter().map(|w| state.mbids.get(w).cloned().flatten()).collect()
    }

    /// the recordings similar to this one, most similar first, from the session's memory when
    /// asked before. `None` when it couldn't be asked.
    pub async fn similar(&self, mbid: &str, deadline: Instant) -> Option<Vec<Recording>> {
        if let Some((_, known)) = self.state.lock().unwrap().similar.iter().find(|(m, _)| m == mbid) {
            return Some(known.clone());
        }
        let mut url = url::Url::parse(&format!("{}/similar-recordings/json", self.base)).ok()?;
        url.query_pairs_mut().append_pair("recording_mbids", mbid).append_pair("algorithm", ALGORITHM);
        let request = self.http().get(url);
        let body = self.send(request, deadline).await?;
        let found: Vec<Recording> = body.as_array().map(|a| a.iter().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect()).unwrap_or_default();
        let mut state = self.state.lock().unwrap();
        state.similar.push((mbid.to_string(), found.clone()));
        let over = state.similar.len().saturating_sub(KEEP_MAX);
        state.similar.drain(..over);
        Some(found)
    }
}
