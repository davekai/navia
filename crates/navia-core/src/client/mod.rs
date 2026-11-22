//! OpenSubsonic client for Navidrome with token authentication.
//!
//! Reads share one path with limited retries. Writes go through another that checks
//! [`WritePolicy`] first, so a [`WritePolicy::ReadOnly`] client can't change the server.

mod auth;
mod form;
pub mod models;
pub mod native;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;
use url::Url;

pub use auth::Credentials;
pub use form::FormPost;
pub use models::*;

use crate::error::{Error, Result};

const API_VERSION: &str = "1.16.1";
const READ_ATTEMPTS: usize = 3;

/// Whether this client may call endpoints that change server state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WritePolicy {
    Allowed,
    ReadOnly,
}

/// the offline switch: while it's on, nothing goes to the server. every request and server URL
/// fails as if the server couldn't be reached, before anything is sent. shared by a client, its
/// copies and the streams and downloads made from it.
#[derive(Clone, Debug, Default)]
pub struct OfflineSwitch(Arc<AtomicBool>);

impl OfflineSwitch {
    pub fn set(&self, on: bool) {
        self.0.store(on, Ordering::Release);
    }

    pub fn is_on(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// the error a request gets while the switch is on: the same as an unreachable server.
    pub fn check(&self) -> Result<()> {
        if self.is_on() { Err(Error::Network(OFFLINE.to_string())) } else { Ok(()) }
    }
}

/// the network error's text while the offline switch is on.
pub const OFFLINE: &str = "offline";

/// the client name Navia reports to the server (the `c` parameter). older builds sent "navia", so
/// recognise it case-insensitively ([`is_navia_client`]).
pub const CLIENT_NAME: &str = "Navia";
/// The app's name wherever people read it. Only the wordmark is lowercase.
pub const APP_DISPLAY_NAME: &str = "Navia";
/// The app ID on Linux: the Wayland app_id, the desktop file's and icon's name, the Flatpak ID and
/// MPRIS's DesktopEntry. `APP_ID` in packaging/app.env must match; build-release.sh checks.
pub const APP_ID: &str = "com.davekai.Navia";

/// A client name without the listen-along marks Navia appends while sharing.
fn bare_client_name(name: &str) -> &str {
    name.trim_end_matches(crate::listen_along::MARKS).trim()
}

/// Whether a client name is Navia's, in any casing and with or without the listen-along marks.
pub fn is_navia_client(name: &str) -> bool {
    bare_client_name(name).eq_ignore_ascii_case(CLIENT_NAME)
}

/// A client name for display: "Navia" for any form of Navia's own, others without the mark.
pub fn client_display_name(name: &str) -> String {
    if is_navia_client(name) { APP_DISPLAY_NAME.to_string() } else { bare_client_name(name).to_string() }
}

static APP_VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Sets the app's release version, once at launch. Any build suffix is dropped because
/// Navidrome makes a new player for every new User-Agent.
pub fn set_app_version(version: &str) {
    let release = version.trim().split(['+', ' ']).next().unwrap_or_default();
    let _ = APP_VERSION.set(release.to_string());
}

/// The platform in the User-Agent.
pub const fn platform_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else {
        "Unix"
    }
}

/// The User-Agent for every request, such as "Navia/0.1.0 (Linux)". Navidrome keys players on
/// it, so it only changes with a release.
pub fn user_agent() -> String {
    let version = APP_VERSION.get().filter(|v| !v.is_empty()).map(String::as_str).unwrap_or(env!("CARGO_PKG_VERSION"));
    format!("{APP_DISPLAY_NAME}/{version} ({})", platform_name())
}

#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub base_url: String,
    pub credentials: Credentials,
    pub client_name: String,
    pub timeout: Duration,
    pub write_policy: WritePolicy,
}

impl ClientConfig {
    pub fn new(base_url: impl Into<String>, credentials: Credentials) -> Self {
        Self {
            base_url: base_url.into(),
            credentials,
            client_name: CLIENT_NAME.to_string(),
            timeout: Duration::from_secs(60),
            write_policy: WritePolicy::Allowed,
        }
    }

    pub fn read_only(mut self) -> Self {
        self.write_policy = WritePolicy::ReadOnly;
        self
    }
}

/// What a `star` or `unstar` call applies to.
#[derive(Clone, Copy, Debug)]
pub enum StarTarget<'a> {
    Song(&'a str),
    Album(&'a str),
    Artist(&'a str),
}

impl StarTarget<'_> {
    fn param(self) -> (&'static str, String) {
        match self {
            StarTarget::Song(id) => ("id", id.to_string()),
            StarTarget::Album(id) => ("albumId", id.to_string()),
            StarTarget::Artist(id) => ("artistId", id.to_string()),
        }
    }
}

/// Selects the artist for `getTopSongs`. `ById` needs the `topSongsByArtistId` extension,
/// see [`crate::capabilities::Capabilities`].
#[derive(Clone, Debug)]
pub enum TopSongsBy {
    Name(String),
    Id(String),
}

/// A `reportPlayback` state (extension `playbackReport`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    /// A song is about to play or play again, starting a playback session.
    Starting,
    Playing,
    Paused,
    Stopped,
}

impl PlaybackState {
    pub fn as_str(self) -> &'static str {
        match self {
            PlaybackState::Starting => "starting",
            PlaybackState::Playing => "playing",
            PlaybackState::Paused => "paused",
            PlaybackState::Stopped => "stopped",
        }
    }
}

/// Which search3 list a sync page requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchKind {
    Artist,
    Album,
    Song,
}

#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    rest_base: Url,
    config: ClientConfig,
    /// Starts from `ClientConfig::write_policy` and can change while the app runs.
    writes_allowed: AtomicBool,
    /// Whether playback reports go out under the share-marked client name.
    sharing: AtomicBool,
    /// While sharing, whether listeners may control this player.
    control: AtomicBool,
    following: AtomicBool,
    queued: AtomicBool,
    /// Navidrome's native API session, kept so pages that read it don't log in on every visit,
    /// and shared with the copies made by [`Client::copy_with_policy`].
    native: Arc<native::NativeShared>,
    /// The server's public web configuration (`window.__APP_CONFIG__` on `/app/`) once read:
    /// `Some(None)` when the page answered without one. Unset until a read got an answer.
    public_config: std::sync::Mutex<Option<Option<Arc<Value>>>>,
    offline: OfflineSwitch,
    form: FormPost,
}

impl Client {
    pub fn new(config: ClientConfig) -> Result<Self> {
        Self::with_native(config, Default::default(), OfflineSwitch::default(), None)
    }

    /// a copy for the same account with its own write policy and playback marks, sharing this
    /// client's native session, so it neither logs in again nor ignores a wait the server asked for.
    pub fn copy_with_policy(&self, policy: WritePolicy) -> Result<Self> {
        let mut config = self.config().clone();
        config.write_policy = policy;
        Self::with_native(config, self.inner.native.clone(), self.inner.offline.clone(), Some(self.inner.form.clone()))
    }

    fn with_native(config: ClientConfig, native: Arc<native::NativeShared>, offline: OfflineSwitch, form: Option<FormPost>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .user_agent(user_agent())
            .build()?;
        let mut base = config.base_url.trim_end_matches('/').to_string();
        base.push_str("/rest/");
        let rest_base = Url::parse(&base).map_err(|e| Error::InvalidUrl(e.to_string()))?;
        let writes_allowed = AtomicBool::new(config.write_policy == WritePolicy::Allowed);
        let form = form.unwrap_or_else(|| FormPost::new(rest_base.to_string(), offline.clone()));
        Ok(Self { inner: Arc::new(Inner { http, rest_base, config, writes_allowed, sharing: AtomicBool::new(false), control: AtomicBool::new(false), following: AtomicBool::new(false), queued: AtomicBool::new(false), native, public_config: Default::default(), offline, form }) })
    }

    /// turns the offline switch on or off for this client and every copy of it.
    pub fn set_offline(&self, on: bool) {
        self.inner.offline.set(on);
    }

    pub fn is_offline(&self) -> bool {
        self.inner.offline.is_on()
    }

    /// the switch itself, for HTTP clients outside this one that talk to the same server.
    pub fn offline_switch(&self) -> OfflineSwitch {
        self.inner.offline.clone()
    }

    /// how requests carry their parameters, for HTTP clients outside this one that fetch this
    /// client's URLs.
    pub fn form_post(&self) -> FormPost {
        self.inner.form.clone()
    }

    /// a request for one of this client's URLs (cover art, a stream) with its own HTTP client.
    pub(crate) async fn fetch(&self, url: &str) -> reqwest::RequestBuilder {
        self.inner.form.request(&self.inner.http, url).await
    }

    pub fn credentials(&self) -> &Credentials {
        &self.inner.config.credentials
    }

    pub fn base_url(&self) -> &str {
        &self.inner.config.base_url
    }

    pub(crate) fn public_config_slot(&self) -> &std::sync::Mutex<Option<Option<Arc<Value>>>> {
        &self.inner.public_config
    }

    pub(crate) fn config(&self) -> &ClientConfig {
        &self.inner.config
    }

    pub fn write_policy(&self) -> WritePolicy {
        if self.inner.writes_allowed.load(Ordering::Acquire) { WritePolicy::Allowed } else { WritePolicy::ReadOnly }
    }

    /// Changes whether this client and all its copies may write.
    pub fn set_write_policy(&self, policy: WritePolicy) {
        self.inner.writes_allowed.store(policy == WritePolicy::Allowed, Ordering::Release);
    }

    /// Marks playback reports as shared, for every copy of this client. Only the client name
    /// changes, the reports go out either way.
    pub fn set_sharing(&self, on: bool) {
        self.inner.sharing.store(on, Ordering::Release);
    }

    pub fn sharing(&self) -> bool {
        self.inner.sharing.load(Ordering::Acquire)
    }

    /// "Let listeners control playback", whether this player follows someone now, and whether it
    /// has a song queued for the listening session: the reports' client name says so to the other
    /// side (only while sharing).
    pub fn set_listen_marks(&self, control: bool, following: bool, queued: bool) {
        self.inner.control.store(control, Ordering::Release);
        self.inner.following.store(following, Ordering::Release);
        self.inner.queued.store(queued, Ordering::Release);
    }

    /// The client name playback reports carry, with the listen-along marks that apply.
    pub fn report_client_name(&self) -> String {
        use crate::listen_along::{CONTROL_MARK, FOLLOW_MARK, QUEUE_MARK, SHARE_MARK};
        let mut name = self.inner.config.client_name.clone();
        if self.sharing() {
            name.push(SHARE_MARK);
            if self.inner.control.load(Ordering::Acquire) {
                name.push(CONTROL_MARK);
            }
            if self.inner.following.load(Ordering::Acquire) {
                name.push(FOLLOW_MARK);
            }
            if self.inner.queued.load(Ordering::Acquire) {
                name.push(QUEUE_MARK);
            }
        }
        name
    }

    /// full URL for an endpoint with every parameter, credentials included, in the query. never
    /// log it. requests Navia sends go through [`FormPost`]; only a URL handed to something
    /// outside Navia is fetched as it is.
    fn endpoint_url(&self, endpoint: &str, params: &[(&str, String)]) -> Result<Url> {
        self.endpoint_url_as(endpoint, params, &self.inner.config.client_name)
    }

    /// [`Self::endpoint_url`] with another client name (`c`).
    fn endpoint_url_as(&self, endpoint: &str, params: &[(&str, String)], client_name: &str) -> Result<Url> {
        self.inner.offline.check()?;
        let cfg = &self.inner.config;
        let (token, salt) = cfg.credentials.token();
        let mut url = self
            .inner
            .rest_base
            .join(endpoint)
            .map_err(|e| Error::InvalidUrl(e.to_string()))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("u", cfg.credentials.username());
            q.append_pair("t", &token);
            q.append_pair("s", &salt);
            q.append_pair("v", API_VERSION);
            q.append_pair("c", client_name);
            q.append_pair("f", "json");
            for (k, v) in params {
                q.append_pair(k, v);
            }
        }
        Ok(url)
    }

    async fn request(&self, endpoint: &'static str, params: &[(&str, String)]) -> Result<Value> {
        self.request_as(endpoint, params, &self.inner.config.client_name).await
    }

    async fn request_as(&self, endpoint: &'static str, params: &[(&str, String)], client_name: &str) -> Result<Value> {
        let url = self.endpoint_url_as(endpoint, params, client_name)?;
        let response = self.fetch(url.as_str()).await.send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::HttpStatus { status: status.as_u16(), endpoint: endpoint.to_string() });
        }
        let body = response.bytes().await?;
        parse_envelope(endpoint, &body)
    }

    /// A read. Retries transient failures with a short backoff.
    async fn read(&self, endpoint: &'static str, params: &[(&str, String)]) -> Result<Value> {
        // not retried: waiting wouldn't turn the switch off
        self.inner.offline.check()?;
        let mut last = None;
        for attempt in 0..READ_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(200 * (1 << attempt))).await;
            }
            match self.request(endpoint, params).await {
                Ok(value) => return Ok(value),
                Err(e) if e.is_transient() => last = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last.expect("at least one attempt ran"))
    }

    /// A write. Not retried here, the play reporter handles scrobble retries.
    async fn write(&self, endpoint: &'static str, params: &[(&str, String)]) -> Result<Value> {
        if self.write_policy() == WritePolicy::ReadOnly {
            return Err(Error::WritesDisabled { endpoint: endpoint.to_string() });
        }
        self.request(endpoint, params).await
    }

    /// A playback report: a write under [`Self::report_client_name`].
    async fn write_report(&self, endpoint: &'static str, params: &[(&str, String)]) -> Result<Value> {
        if self.write_policy() == WritePolicy::ReadOnly {
            return Err(Error::WritesDisabled { endpoint: endpoint.to_string() });
        }
        self.request_as(endpoint, params, &self.report_client_name()).await
    }

    /// A write sent as a POST form, for parameter lists too long for a URL. Auth stays in the URL.
    async fn write_form(&self, endpoint: &'static str, params: &[(&str, String)]) -> Result<Value> {
        if self.write_policy() == WritePolicy::ReadOnly {
            return Err(Error::WritesDisabled { endpoint: endpoint.to_string() });
        }
        let whole = self.endpoint_url(endpoint, params)?;
        let url = self.endpoint_url(endpoint, &[])?;
        // Own scope because the serializer isn't Send and can't live across an await.
        let body = {
            let mut form = url::form_urlencoded::Serializer::new(String::new());
            for (k, v) in params {
                form.append_pair(k, v);
            }
            form.finish()
        };
        let response = self.inner.form.post_form(&self.inner.http, whole.as_str(), (url.as_str(), body)).await.send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::HttpStatus { status: status.as_u16(), endpoint: endpoint.to_string() });
        }
        let bytes = response.bytes().await?;
        parse_envelope(endpoint, &bytes)
    }

    async fn read_as<T: DeserializeOwned>(&self, endpoint: &'static str, key: &str, params: &[(&str, String)]) -> Result<T> {
        let value = self.read(endpoint, params).await?;
        field(endpoint, &value, key)
    }

    /// A `ping` request with no retries or error mapping, for the sign-in check in [`crate::connect`].
    pub(crate) async fn ping_request(&self) -> Result<reqwest::RequestBuilder> {
        let url = self.endpoint_url("ping", &[])?;
        Ok(self.fetch(url.as_str()).await)
    }

    // ---- reads --------------------------------------------------------------------------

    pub async fn ping(&self) -> Result<ServerInfo> {
        let value = self.read("ping", &[]).await?;
        Ok(server_info(&value))
    }

    /// `getOpenSubsonicExtensions`. Servers without OpenSubsonic may answer with an error, which
    /// callers treat as unknown.
    pub async fn open_subsonic_extensions(&self) -> Result<Vec<Extension>> {
        let value = self.read("getOpenSubsonicExtensions", &[]).await?;
        if value.get("openSubsonicExtensions").is_none() {
            self.inner.form.set(false);
            return Ok(Vec::new());
        }
        let list: Vec<Extension> = field("getOpenSubsonicExtensions", &value, "openSubsonicExtensions")?;
        self.inner.form.set(list.iter().any(|e| e.name == "formPost"));
        Ok(list)
    }

    pub async fn music_folders(&self) -> Result<Vec<MusicFolder>> {
        #[derive(serde::Deserialize)]
        struct R {
            #[serde(default, rename = "musicFolder", deserialize_with = "models::lenient_vec")]
            folders: Vec<MusicFolder>,
        }
        let r: R = self.read_as("getMusicFolders", "musicFolders", &[]).await?;
        Ok(r.folders)
    }

    pub async fn scan_status(&self) -> Result<ScanStatus> {
        self.read_as("getScanStatus", "scanStatus", &[]).await
    }

    pub async fn user(&self) -> Result<User> {
        let username = self.credentials().username().to_string();
        self.read_as("getUser", "user", &[("username", username)]).await
    }

    pub async fn shares(&self) -> Result<Vec<Share>> {
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct R {
            #[serde(deserialize_with = "models::lenient_vec")]
            share: Vec<Share>,
        }
        let value = self.read("getShares", &[]).await?;
        if value.get("shares").is_none() {
            return Ok(Vec::new());
        }
        let r: R = field("getShares", &value, "shares")?;
        Ok(r.share)
    }

    /// `createShare` for `ids` of one kind. `expires` is Unix ms, `None` for the server default.
    pub async fn create_share(&self, ids: &[String], description: &str, expires: Option<u64>, downloadable: Option<bool>) -> Result<Share> {
        let mut params: Vec<(&str, String)> = ids.iter().map(|id| ("id", id.clone())).collect();
        if !description.trim().is_empty() {
            params.push(("description", description.trim().to_string()));
        }
        if let Some(ms) = expires {
            params.push(("expires", ms.to_string()));
        }
        if let Some(on) = downloadable {
            params.push(("downloadable", on.to_string()));
        }
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct R {
            #[serde(deserialize_with = "models::lenient_vec")]
            share: Vec<Share>,
        }
        let value = self.write("createShare", &params).await?;
        let r: R = field("createShare", &value, "shares")?;
        r.share.into_iter().next().ok_or_else(|| Error::Decode { endpoint: "createShare".to_string(), detail: "no share in the answer".to_string() })
    }

    /// `updateShare`. An empty description clears it. Expiry and downloads change only when given.
    pub async fn update_share(&self, id: &str, description: &str, expires: Option<u64>, downloadable: Option<bool>) -> Result<()> {
        let mut params: Vec<(&str, String)> = vec![("id", id.to_string()), ("description", description.trim().to_string())];
        if let Some(ms) = expires {
            params.push(("expires", ms.to_string()));
        }
        if let Some(on) = downloadable {
            params.push(("downloadable", on.to_string()));
        }
        self.write("updateShare", &params).await.map(|_| ())
    }

    /// `deleteShare`: the link stops working for everyone.
    pub async fn delete_share(&self, id: &str) -> Result<()> {
        self.write("deleteShare", &[("id", id.to_string())]).await.map(|_| ())
    }

    pub async fn artists(&self, music_folder_id: Option<&str>) -> Result<Artists> {
        let mut params = Vec::new();
        push_opt(&mut params, "musicFolderId", music_folder_id);
        self.read_as("getArtists", "artists", &params).await
    }

    pub async fn artist(&self, id: &str) -> Result<Artist> {
        self.read_as("getArtist", "artist", &[("id", id.to_string())]).await
    }

    pub async fn album(&self, id: &str) -> Result<Album> {
        self.read_as("getAlbum", "album", &[("id", id.to_string())]).await
    }

    pub async fn song(&self, id: &str) -> Result<Song> {
        self.read_as("getSong", "song", &[("id", id.to_string())]).await
    }

    /// `getAlbumList2`. The server silently caps `size` at 500.
    pub async fn album_list2(&self, opts: &AlbumListOptions) -> Result<Vec<Album>> {
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct R {
            #[serde(deserialize_with = "models::lenient_vec")]
            album: Vec<Album>,
        }
        let mut params = vec![
            ("type", opts.list_type.clone()),
            ("size", opts.size.to_string()),
            ("offset", opts.offset.to_string()),
        ];
        push_opt(&mut params, "musicFolderId", opts.music_folder_id.as_deref());
        push_opt(&mut params, "genre", opts.genre.as_deref());
        if let Some(y) = opts.from_year {
            params.push(("fromYear", y.to_string()));
        }
        if let Some(y) = opts.to_year {
            params.push(("toYear", y.to_string()));
        }
        let r: R = self.read_as("getAlbumList2", "albumList2", &params).await?;
        Ok(r.album)
    }

    /// Plain `search3`, simple autocomplete. Only for when there is no local library.
    pub async fn search3(&self, query: &str, artist_count: u32, album_count: u32, song_count: u32, music_folder_id: Option<&str>) -> Result<(Vec<Artist>, Vec<Album>, Vec<Song>)> {
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct R {
            #[serde(deserialize_with = "models::lenient_vec")]
            artist: Vec<Artist>,
            #[serde(deserialize_with = "models::lenient_vec")]
            album: Vec<Album>,
            #[serde(deserialize_with = "models::lenient_vec")]
            song: Vec<Song>,
        }
        let mut params = vec![
            ("query", query.to_string()),
            ("artistCount", artist_count.to_string()),
            ("albumCount", album_count.to_string()),
            ("songCount", song_count.to_string()),
        ];
        push_opt(&mut params, "musicFolderId", music_folder_id);
        let value = self.read("search3", &params).await?;
        if value.get("searchResult3").is_none() {
            return Ok((Vec::new(), Vec::new(), Vec::new()));
        }
        let r: R = field("search3", &value, "searchResult3")?;
        Ok((r.artist, r.album, r.song))
    }

    /// One page of the whole library via `search3` with an empty query. The page size isn't
    /// documented, so advance by `raw_count` and stop only at an empty page.
    pub async fn search_page<T: DeserializeOwned>(&self, kind: SearchKind, music_folder_id: Option<&str>, offset: u32, count: u32) -> Result<SearchPage<T>> {
        let key = match kind {
            SearchKind::Artist => "artist",
            SearchKind::Album => "album",
            SearchKind::Song => "song",
        };
        let zero = || "0".to_string();
        let mut params = vec![("query", String::new())];
        for (name, active) in [("artist", SearchKind::Artist), ("album", SearchKind::Album), ("song", SearchKind::Song)] {
            let (count_key, offset_key) = match name {
                "artist" => ("artistCount", "artistOffset"),
                "album" => ("albumCount", "albumOffset"),
                _ => ("songCount", "songOffset"),
            };
            if kind == active {
                params.push((count_key, count.to_string()));
                params.push((offset_key, offset.to_string()));
            } else {
                params.push((count_key, zero()));
            }
        }
        push_opt(&mut params, "musicFolderId", music_folder_id);
        let value = self.read("search3", &params).await?;
        let raw: Vec<Value> = value
            .get("searchResult3")
            .and_then(|r| r.get(key))
            .and_then(|l| l.as_array())
            .cloned()
            .unwrap_or_default();
        let raw_count = raw.len();
        let mut items = Vec::with_capacity(raw_count);
        for v in raw {
            match serde_json::from_value::<T>(v) {
                Ok(item) => items.push(item),
                Err(e) => tracing::warn!("skipping {key} that failed to parse: {e}"),
            }
        }
        Ok(SearchPage { items, raw_count })
    }

    pub async fn playlists(&self) -> Result<Vec<Playlist>> {
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct R {
            #[serde(deserialize_with = "models::lenient_vec")]
            playlist: Vec<Playlist>,
        }
        let value = self.read("getPlaylists", &[]).await?;
        if value.get("playlists").is_none() {
            return Ok(Vec::new());
        }
        let r: R = field("getPlaylists", &value, "playlists")?;
        Ok(r.playlist)
    }

    pub async fn playlist(&self, id: &str) -> Result<Playlist> {
        self.read_as("getPlaylist", "playlist", &[("id", id.to_string())]).await
    }

    /// `getTopSongs`. Depends on the server's external integrations and may return nothing.
    pub async fn top_songs(&self, by: &TopSongsBy, count: u32) -> Result<Vec<Song>> {
        let mut params = vec![("count", count.to_string())];
        match by {
            TopSongsBy::Name(name) => params.push(("artist", name.clone())),
            TopSongsBy::Id(id) => params.push(("id", id.clone())),
        }
        self.song_list("getTopSongs", "topSongs", &params).await
    }

    /// An artist's top songs, by id when `by_id` (no mix-up between same-named artists), else by
    /// name. Falls back to the name if the id lookup fails.
    pub async fn artist_top_songs(&self, artist_id: &str, artist_name: &str, by_id: bool, count: u32) -> Result<Vec<Song>> {
        if by_id && !artist_id.is_empty() {
            match self.top_songs(&TopSongsBy::Id(artist_id.to_string()), count).await {
                Ok(songs) => return Ok(songs),
                Err(e) => tracing::debug!("getTopSongs by id failed, trying the name: {e}"),
            }
        }
        self.top_songs(&TopSongsBy::Name(artist_name.to_string()), count).await
    }

    /// `getRandomSongs`: up to `size` random songs, optionally from one genre.
    pub async fn random_songs(&self, size: u32, genre: Option<&str>) -> Result<Vec<Song>> {
        let mut params = vec![("size", size.to_string())];
        if let Some(g) = genre {
            params.push(("genre", g.to_string()));
        }
        self.song_list("getRandomSongs", "randomSongs", &params).await
    }

    /// `getSimilarSongs2`. Depends on external integrations and can take seconds on first use.
    pub async fn similar_songs2(&self, id: &str, count: u32) -> Result<Vec<Song>> {
        self.song_list("getSimilarSongs2", "similarSongs2", &[("id", id.to_string()), ("count", count.to_string())]).await
    }

    async fn song_list(&self, endpoint: &'static str, key: &str, params: &[(&str, String)]) -> Result<Vec<Song>> {
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct R {
            #[serde(deserialize_with = "models::lenient_vec")]
            song: Vec<Song>,
        }
        let value = self.read(endpoint, params).await?;
        if value.get(key).is_none() {
            return Ok(Vec::new());
        }
        let r: R = field(endpoint, &value, key)?;
        Ok(r.song)
    }

    pub async fn artist_info2(&self, id: &str, similar_count: Option<u32>) -> Result<ArtistInfo> {
        let mut params = vec![("id", id.to_string())];
        if let Some(c) = similar_count {
            params.push(("count", c.to_string()));
        }
        let value = self.read("getArtistInfo2", &params).await?;
        if value.get("artistInfo2").is_none() {
            return Ok(ArtistInfo::default());
        }
        field("getArtistInfo2", &value, "artistInfo2")
    }

    /// Synced lyrics by song id (`songLyrics`). `enhanced` asks for word timing and the other
    /// layers, which needs version 2.
    pub async fn lyrics_by_song_id(&self, id: &str, enhanced: bool) -> Result<Vec<StructuredLyrics>> {
        #[derive(serde::Deserialize, Default)]
        #[serde(default, rename_all = "camelCase")]
        struct R {
            #[serde(deserialize_with = "models::lenient_vec")]
            structured_lyrics: Vec<StructuredLyrics>,
        }
        let mut params = vec![("id", id.to_string())];
        if enhanced {
            params.push(("enhanced", "true".to_string()));
        }
        let value = self.read("getLyricsBySongId", &params).await?;
        if value.get("lyricsList").is_none() {
            return Ok(Vec::new());
        }
        let r: R = field("getLyricsBySongId", &value, "lyricsList")?;
        Ok(r.structured_lyrics)
    }

    /// Classic unsynced lyrics matched by artist and title.
    pub async fn lyrics(&self, artist: &str, title: &str) -> Result<PlainLyrics> {
        let value = self.read("getLyrics", &[("artist", artist.to_string()), ("title", title.to_string())]).await?;
        if value.get("lyrics").is_none() {
            return Ok(PlainLyrics::default());
        }
        field("getLyrics", &value, "lyrics")
    }

    /// `getAlbumInfo2`: notes and links from the server's external sources. Empty without them.
    pub async fn album_info2(&self, id: &str) -> Result<AlbumInfo> {
        let value = self.read("getAlbumInfo2", &[("id", id.to_string())]).await?;
        if value.get("albumInfo").is_none() {
            return Ok(AlbumInfo::default());
        }
        field("getAlbumInfo2", &value, "albumInfo")
    }

    pub async fn internet_radio_stations(&self) -> Result<Vec<InternetRadioStation>> {
        #[derive(serde::Deserialize, Default)]
        #[serde(default, rename_all = "camelCase")]
        struct R {
            #[serde(deserialize_with = "models::lenient_vec")]
            internet_radio_station: Vec<InternetRadioStation>,
        }
        let value = self.read("getInternetRadioStations", &[]).await?;
        if value.get("internetRadioStations").is_none() {
            return Ok(Vec::new());
        }
        let r: R = field("getInternetRadioStations", &value, "internetRadioStations")?;
        Ok(r.internet_radio_station)
    }

    /// `getPlayQueue`. `None` when the server has no saved queue.
    pub async fn play_queue(&self) -> Result<Option<PlayQueue>> {
        let value = self.read("getPlayQueue", &[]).await?;
        if value.get("playQueue").is_none() {
            return Ok(None);
        }
        Ok(Some(field("getPlayQueue", &value, "playQueue")?))
    }

    /// `getNowPlaying` as raw entries, for checks.
    pub async fn now_playing(&self) -> Result<Vec<Value>> {
        let value = self.read("getNowPlaying", &[]).await?;
        Ok(value.get("nowPlaying").and_then(|n| n.get("entry")).and_then(Value::as_array).cloned().unwrap_or_default())
    }

    /// `getPlayQueueByIndex` (extension `indexBasedQueue`). `None` when nothing is saved.
    pub async fn play_queue_by_index(&self) -> Result<Option<PlayQueueByIndex>> {
        let value = self.read("getPlayQueueByIndex", &[]).await?;
        if value.get("playQueueByIndex").is_none() {
            return Ok(None);
        }
        Ok(Some(field("getPlayQueueByIndex", &value, "playQueueByIndex")?))
    }

    /// `getStarred2`: everything this user has starred.
    pub async fn starred2(&self) -> Result<Starred> {
        let value = self.read("getStarred2", &[]).await?;
        if value.get("starred2").is_none() {
            return Ok(Starred::default());
        }
        field("getStarred2", &value, "starred2")
    }

    pub async fn genres(&self) -> Result<Vec<Genre>> {
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct R {
            #[serde(deserialize_with = "models::lenient_vec")]
            genre: Vec<Genre>,
        }
        let value = self.read("getGenres", &[]).await?;
        if value.get("genres").is_none() {
            return Ok(Vec::new());
        }
        let r: R = field("getGenres", &value, "genres")?;
        Ok(r.genre)
    }

    // ---- URLs ----------------------------------------------------------------------------

    /// `getCoverArt` URL. Size is in pixels, `None` for the original.
    pub fn cover_art_url(&self, cover_art_id: &str, size: Option<u32>) -> Result<String> {
        let mut params = vec![("id", cover_art_id.to_string())];
        if let Some(s) = size {
            params.push(("size", s.to_string()));
        }
        Ok(self.endpoint_url("getCoverArt", &params)?.to_string())
    }

    /// `getTranscodeDecision` URL (POST, extension `transcoding`).
    pub fn transcode_decision_url(&self, id: &str) -> Result<String> {
        Ok(self.endpoint_url("getTranscodeDecision", &[("mediaId", id.to_string()), ("mediaType", "song".to_string())])?.to_string())
    }

    /// `getTranscodeStream` URL, starting `offset_s` whole seconds in.
    pub fn transcode_stream_url(&self, id: &str, params: &str, offset_s: u32) -> Result<String> {
        Ok(self.endpoint_url("getTranscodeStream", &[("mediaId", id.to_string()), ("mediaType", "song".to_string()), ("offset", offset_s.to_string()), ("transcodeParams", params.to_string())])?.to_string())
    }

    /// `getOpenSubsonicExtensions` URL, for a blocking check.
    pub fn extensions_url(&self) -> Result<String> {
        Ok(self.endpoint_url("getOpenSubsonicExtensions", &[])?.to_string())
    }

    /// `stream` URL. Streaming doesn't count as a play, see [`crate::reporting`].
    pub fn stream_url(&self, id: &str, opts: &StreamOptions) -> Result<String> {
        let mut params = vec![("id", id.to_string())];
        if let Some(b) = opts.max_bit_rate {
            params.push(("maxBitRate", b.to_string()));
        }
        if let Some(f) = &opts.format {
            params.push(("format", f.clone()));
        }
        if let Some(t) = opts.time_offset {
            params.push(("timeOffset", t.to_string()));
        }
        Ok(self.endpoint_url("stream", &params)?.to_string())
    }

    // ---- writes (gated by WritePolicy) ---------------------------------------------------

    /// `scrobble`. Only `submission = true` counts a play, `false` is a now-playing report.
    /// Use [`crate::reporting::PlayReporter`] instead of calling this.
    pub async fn scrobble(&self, id: &str, submission: bool, time_ms: Option<u64>) -> Result<()> {
        let mut params = vec![("id", id.to_string()), ("submission", submission.to_string())];
        if let Some(t) = time_ms {
            params.push(("time", t.to_string()));
        }
        // A now-playing report carries the share mark while sharing.
        if submission { self.write("scrobble", &params).await } else { self.write_report("scrobble", &params).await }.map(|_| ())
    }

    /// `star`: add to the signed-in user's favorites.
    pub async fn star(&self, target: StarTarget<'_>) -> Result<()> {
        self.write("star", &[target.param()]).await.map(|_| ())
    }

    /// `unstar`: remove from the signed-in user's favorites.
    pub async fn unstar(&self, target: StarTarget<'_>) -> Result<()> {
        self.write("unstar", &[target.param()]).await.map(|_| ())
    }

    /// `createPlaylist` with just a name. `None` if the server doesn't answer with the playlist.
    pub async fn create_playlist(&self, name: &str) -> Result<Option<Playlist>> {
        let response = self.write("createPlaylist", &[("name", name.to_string())]).await?;
        match response.get("playlist") {
            Some(_) => field("createPlaylist", &response, "playlist").map(Some),
            None => Ok(None),
        }
    }

    /// `deletePlaylist`. The server allows it for the owner or an admin.
    pub async fn delete_playlist(&self, id: &str) -> Result<()> {
        self.write("deletePlaylist", &[("id", id.to_string())]).await.map(|_| ())
    }

    /// Replaces the playlist's songs with these, in order, via `createPlaylist` with `playlistId`.
    /// Sent as a POST form so any length fits.
    pub async fn replace_playlist_songs(&self, id: &str, song_ids: &[String]) -> Result<Option<Playlist>> {
        let mut params: Vec<(&str, String)> = vec![("playlistId", id.to_string())];
        params.extend(song_ids.iter().map(|s| ("songId", s.clone())));
        let response = self.write_form("createPlaylist", &params).await?;
        match response.get("playlist") {
            Some(_) => field("createPlaylist", &response, "playlist").map(Some),
            None => Ok(None),
        }
    }

    /// `createPlaylist` with a name and its first songs. Without `form` (`formPost`), keep the
    /// list short.
    pub async fn create_playlist_with(&self, name: &str, song_ids: &[String], form: bool) -> Result<Option<Playlist>> {
        let mut params: Vec<(&str, String)> = vec![("name", name.to_string())];
        params.extend(song_ids.iter().map(|s| ("songId", s.clone())));
        let response = if form { self.write_form("createPlaylist", &params).await? } else { self.write("createPlaylist", &params).await? };
        match response.get("playlist") {
            Some(_) => field("createPlaylist", &response, "playlist").map(Some),
            None => Ok(None),
        }
    }

    /// `updatePlaylist`: appends `add` and removes the 0-based positions in `remove`. Without
    /// `form` (`formPost`), keep the lists short.
    pub async fn update_playlist_songs(&self, id: &str, add: &[String], remove: &[usize], form: bool) -> Result<()> {
        let mut params: Vec<(&str, String)> = vec![("playlistId", id.to_string())];
        params.extend(add.iter().map(|s| ("songIdToAdd", s.clone())));
        params.extend(remove.iter().map(|i| ("songIndexToRemove", i.to_string())));
        if form { self.write_form("updatePlaylist", &params).await } else { self.write("updatePlaylist", &params).await }.map(|_| ())
    }

    /// `updatePlaylist` for details only. Each `Some` field is set, and an empty comment clears it.
    pub async fn update_playlist_details(&self, id: &str, details: &PlaylistDetails) -> Result<()> {
        let mut params: Vec<(&str, String)> = vec![("playlistId", id.to_string())];
        push_opt(&mut params, "name", details.name.as_deref());
        push_opt(&mut params, "comment", details.comment.as_deref());
        if let Some(public) = details.public {
            params.push(("public", public.to_string()));
        }
        self.write("updatePlaylist", &params).await.map(|_| ())
    }

    /// `savePlayQueue`. Prefer [`Self::save_play_queue_by_index`] where the server offers it.
    pub async fn save_play_queue(&self, ids: &[String], current: Option<&str>, position_ms: Option<u64>) -> Result<()> {
        let mut params: Vec<(&str, String)> = ids.iter().map(|id| ("id", id.clone())).collect();
        push_opt(&mut params, "current", current);
        if let Some(p) = position_ms {
            params.push(("position", p.to_string()));
        }
        self.write("savePlayQueue", &params).await.map(|_| ())
    }

    /// `savePlayQueueByIndex`. An empty list clears the saved queue. `form` lets a long queue fit.
    pub async fn save_play_queue_by_index(&self, ids: &[String], current_index: usize, position_ms: u64, form: bool) -> Result<()> {
        let mut params: Vec<(&str, String)> = ids.iter().map(|id| ("id", id.clone())).collect();
        if !ids.is_empty() {
            params.push(("currentIndex", current_index.min(ids.len() - 1).to_string()));
            params.push(("position", position_ms.to_string()));
        }
        if form { self.write_form("savePlayQueueByIndex", &params).await } else { self.write("savePlayQueueByIndex", &params).await }.map(|_| ())
    }

    /// `reportPlayback` for the server's now-playing view. Always sent with `ignoreScrobble=true`
    /// because plays are counted through `scrobble`.
    pub async fn report_playback(&self, id: &str, state: PlaybackState, position_ms: u64) -> Result<()> {
        self.report_playback_as(&self.report_client_name(), id, state, position_ms).await
    }

    /// [`Self::report_playback`] under a given client name: an earlier one, to end its session.
    pub async fn report_playback_as(&self, client_name: &str, id: &str, state: PlaybackState, position_ms: u64) -> Result<()> {
        let params = [
            ("mediaId", id.to_string()),
            ("mediaType", "song".to_string()),
            ("positionMs", position_ms.to_string()),
            ("state", state.as_str().to_string()),
            ("playbackRate", "1.0".to_string()),
            ("ignoreScrobble", "true".to_string()),
        ];
        if self.write_policy() == WritePolicy::ReadOnly {
            return Err(Error::WritesDisabled { endpoint: "reportPlayback".to_string() });
        }
        self.request_as("reportPlayback", &params, client_name).await.map(|_| ())
    }
}

fn push_opt<'a>(params: &mut Vec<(&'a str, String)>, key: &'a str, value: Option<&str>) {
    if let Some(v) = value {
        params.push((key, v.to_string()));
    }
}

/// Pull `subsonic-response` out and turn an error document into `Err`.
fn parse_envelope(endpoint: &str, body: &[u8]) -> Result<Value> {
    let decode = |detail: String| Error::Decode { endpoint: endpoint.to_string(), detail };
    let root: Value = serde_json::from_slice(body).map_err(|e| decode(e.to_string()))?;
    let response = root
        .get("subsonic-response")
        .cloned()
        .ok_or_else(|| decode("missing subsonic-response".to_string()))?;
    match response.get("status").and_then(Value::as_str) {
        Some("ok") => Ok(response),
        _ => {
            let err = response.get("error");
            let code = err.and_then(|e| e.get("code")).and_then(Value::as_i64).unwrap_or(0) as i32;
            let message = err
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unspecified")
                .to_string();
            Err(Error::Subsonic { code, endpoint: endpoint.to_string(), message })
        }
    }
}

fn field<T: DeserializeOwned>(endpoint: &str, response: &Value, key: &str) -> Result<T> {
    let value = response.get(key).cloned().ok_or_else(|| Error::Decode {
        endpoint: endpoint.to_string(),
        detail: format!("missing `{key}`"),
    })?;
    serde_json::from_value(value).map_err(|e| Error::Decode { endpoint: endpoint.to_string(), detail: e.to_string() })
}

pub(crate) fn server_info(response: &Value) -> ServerInfo {
    let text = |key: &str| response.get(key).and_then(Value::as_str).map(str::to_string);
    ServerInfo {
        api_version: text("version").unwrap_or_default(),
        kind: text("type"),
        server_version: text("serverVersion"),
        open_subsonic: response.get("openSubsonic").and_then(Value::as_bool).unwrap_or(false),
    }
}

#[cfg(test)]
mod display_name_tests {
    use super::{CLIENT_NAME, client_display_name, is_navia_client, user_agent};
    use crate::listen_along::{CONTROL_MARK, FOLLOW_MARK, SHARE_MARK};

    /// "Navia" and "navia", in any casing and with any marks, are Navia. Look-alikes aren't.
    #[test]
    fn navia_is_recognised_in_any_casing() {
        assert_eq!(CLIENT_NAME, "Navia");
        for name in ["Navia", "navia", "NAVIA", " Navia ", "navia\u{200B}", &format!("Navia{SHARE_MARK}{CONTROL_MARK}{FOLLOW_MARK}"), &format!("navia{SHARE_MARK}{FOLLOW_MARK}")] {
            assert!(is_navia_client(name), "{name:?}");
            assert_eq!(client_display_name(name), "Navia");
        }
        for name in ["navia-fork", "Navi", "other-client", "", "Navia Player"] {
            assert!(!is_navia_client(name), "{name:?}");
        }
    }

    #[test]
    fn user_agent_names_the_app_version_and_platform() {
        let ua = user_agent();
        assert!(ua.starts_with("Navia/"), "{ua}");
        assert!(ua.ends_with(&format!("({})", super::platform_name())), "{ua}");
    }

    #[test]
    fn navia_reads_as_navia() {
        assert_eq!(client_display_name("navia"), "Navia");
        assert_eq!(client_display_name("NAVIA\u{200B}"), "Navia");
        assert_eq!(client_display_name(" Navia "), "Navia");
        assert_eq!(client_display_name("other-client"), "other-client");
        assert_eq!(client_display_name("Phone\u{200B}"), "Phone");
        assert_eq!(client_display_name("navia-fork"), "navia-fork");
        assert_eq!(client_display_name(""), "");
    }
}
