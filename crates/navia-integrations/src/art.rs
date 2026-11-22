//! Public album art for Discord. Navidrome's artwork is private, so Discord needs a URL from somewhere else. Sources
//! plug in through [`ArtSource`].

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::Url;
use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use serde_json::Value;

use crate::TrackInfo;

#[derive(Debug)]
pub struct ArtError(pub String);

impl fmt::Display for ArtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ArtError {}

impl From<reqwest::Error> for ArtError {
    fn from(error: reqwest::Error) -> Self {
        ArtError(error.to_string())
    }
}

/// Finds a public `https` image for a track's album.
///
/// `Ok(None)` means there is no art and the answer may be cached. `Err` means
/// the lookup failed and may be tried again for a later song.
pub trait ArtSource: Send + Sync {
    fn find(&self, track: &TrackInfo) -> Result<Option<String>, ArtError>;

    /// Like [`ArtSource::find`], but also says whether the cover applies to the whole album, so its other songs can
    /// reuse it.
    fn find_art(&self, track: &TrackInfo) -> Result<Option<Art>, ArtError> {
        Ok(self.find(track)?.map(|url| Art { url, album_wide: true }))
    }

    /// Changes when earlier answers no longer hold, like after a new public address, so they're dropped.
    fn generation(&self) -> u64 {
        0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Art {
    pub url: String,
    /// Found by the album, so it applies to every song on it. False for a song-level match.
    pub album_wide: bool,
}

/// Never finds anything, so Discord always shows the default image.
pub struct NoArt;

impl ArtSource for NoArt {
    fn find(&self, _track: &TrackInfo) -> Result<Option<String>, ArtError> {
        Ok(None)
    }
}

/// The longest image URL sent to Discord. The Social SDK allows 300 characters for `LargeImage`, but Rich Presence over
/// IPC isn't documented, so this stays at the older, safer 256.
pub const MAX_IMAGE_URL_LEN: usize = 256;

/// What the app's own server can answer for Discord art. Only called on the art thread, so never in Private mode, with
/// Discord status off, or in a test run.
pub trait ServerLookup: Send + Sync {
    /// `getAlbumInfo2`'s `largeImageUrl`, Navidrome's public `/share/img/<signed token>` link.
    fn album_image_link(&self, album_id: &str) -> Result<Option<String>, ArtError>;
    /// The song's ISRC, when the app's own copy of the song lacks it.
    fn song_isrc(&self, song_id: &str) -> Result<Option<String>, ArtError>;
}

/// Why a public server address can't be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicAddressError {
    NotAUrl,
    NotHttps,
    /// A user name, password, query or fragment, none of which belong in a link Discord shows.
    ExtraParts,
}

/// Validates the "Public server address" setting. Empty means none. Only `https`, with a host and an optional base
/// path.
pub fn parse_public_address(text: &str) -> Result<Option<Url>, PublicAddressError> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let url = Url::parse(text).map_err(|_| PublicAddressError::NotAUrl)?;
    if url.scheme() != "https" {
        return Err(PublicAddressError::NotHttps);
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(PublicAddressError::NotAUrl);
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err(PublicAddressError::ExtraParts);
    }
    Ok(Some(url))
}

const PUBLIC_IMAGE_PATH: &str = "/share/img/";

/// Moves the server's public image link onto the public address, taking its scheme, host, port and any path. Keeps only
/// the signed token and `size`, so no credentials or Subsonic auth parameters leak. None if it isn't such a link, the
/// address isn't `https`, or the result is too long for Discord.
pub fn public_cover_url(link: &str, public: &Url) -> Option<String> {
    if public.scheme() != "https" || public.host_str().is_none() {
        return None;
    }
    let link = Url::parse(link).ok()?;
    let path = link.path();
    let at = path.find(PUBLIC_IMAGE_PATH)?;
    let token = &path[at + PUBLIC_IMAGE_PATH.len()..];
    // Only a signed token, base64url segments joined by dots.
    if token.is_empty() || !token.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        return None;
    }
    let base = match public.path().trim_end_matches('/') {
        "" => path[..at].trim_end_matches('/'),
        own => own,
    };
    let mut out = public.clone();
    out.set_path(&format!("{base}{PUBLIC_IMAGE_PATH}{token}"));
    out.set_query(None);
    out.set_fragment(None);
    let _ = out.set_username("");
    let _ = out.set_password(None);
    if let Some((_, size)) = link.query_pairs().find(|(k, _)| k == "size").filter(|(_, v)| !v.is_empty() && v.len() <= 5 && v.bytes().all(|b| b.is_ascii_digit())) {
        out.query_pairs_mut().append_pair("size", &size);
    }
    let out = out.to_string();
    (out.len() <= MAX_IMAGE_URL_LEN).then_some(out)
}

/// How long a public address that failed to load is left alone before it's checked again.
const SERVER_RETRY_AFTER: Duration = Duration::from_secs(30 * 60);

/// Host names that are always local, mDNS and loopback. Others like `.lan` or Tailscale's `.ts.net`, which Funnel can
/// expose, are judged by their DNS answers.
const PRIVATE_SUFFIXES: [&str; 2] = [".local", ".localhost"];

/// Whether an address is only reachable from a private network: loopback, private and link-local ranges, CGNAT and
/// Tailscale (100.64.0.0/10), unique-local IPv6, or anything that isn't a unicast internet address.
pub fn ip_is_private(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast() || v4.is_multicast() || v4.is_documentation() || a == 0 || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return ip_is_private(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() || v6.is_unique_local() || v6.is_unicast_link_local() || (first == 0x2001 && v6.segments()[1] == 0x0db8)
        }
    }
}

/// Whether the connected server's address may be public for Discord covers, checked without DNS. It must be `https`
/// with a host that isn't a private IP or a local-network name.
pub fn server_address_may_be_public(url: &Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    match url_ip(url) {
        Some(ip) => !ip_is_private(ip),
        None => {
            let name = url.host_str().unwrap_or("").trim_end_matches('.').to_ascii_lowercase();
            !name.is_empty() && name != "localhost" && !PRIVATE_SUFFIXES.iter().any(|s| name.ends_with(s))
        }
    }
}

/// The URL's host when it's an IP address. IPv6 hosts come bracketed.
fn url_ip(url: &Url) -> Option<std::net::IpAddr> {
    url.host_str()?.trim_start_matches('[').trim_end_matches(']').parse().ok()
}

/// The connected server's address, if it's public. It must pass [`server_address_may_be_public`] and resolve to at
/// least one public address, so split DNS at home doesn't count. Blocks on the system resolver, so never call it on a
/// UI thread.
pub fn public_server_address(server: &str) -> Option<Url> {
    let mut url = Url::parse(server.trim()).ok()?;
    if !server_address_may_be_public(&url) {
        return None;
    }
    if url_ip(&url).is_none() {
        use std::net::ToSocketAddrs;
        let name = url.host_str()?;
        let addrs = (name, url.port_or_known_default().unwrap_or(443)).to_socket_addrs().ok()?;
        if !any_public(addrs.map(|a| a.ip())) {
            return None;
        }
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    Some(url)
}

/// Settings > Discord "Covers from". Navia can't know whether Discord can reach the server, so `Automatic` guesses from
/// the address and the others let the user decide.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CoverSource {
    /// The server when its address looks public ([`public_server_address`]), else the external sources.
    #[default]
    Automatic,
    /// Always the server's own cover link.
    Server,
    /// Always the external sources, by ISRC, then album, then song.
    Search,
}

/// The connected server's address used without checking whether it's public, for [`CoverSource::Server`]. Still only
/// `https`, with no user name, query or fragment.
fn server_address_as_is(server: &str) -> Option<Url> {
    let mut url = Url::parse(server.trim()).ok()?;
    if url.scheme() != "https" || url.host_str().is_none_or(str::is_empty) {
        return None;
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    Some(url)
}

/// Whether a name's DNS answers include a public address. No answers counts as private.
fn any_public(mut ips: impl Iterator<Item = std::net::IpAddr>) -> bool {
    ips.any(|ip| !ip_is_private(ip))
}

/// Which address covers come from. `Search` never uses one. Otherwise a filled-in "Public server address" wins, then
/// the connected server's address. For `Automatic` that's only when the caller found it public.
pub fn cover_address(source: CoverSource, custom: Option<Url>, connected: Option<Url>) -> Option<Url> {
    match source {
        CoverSource::Search => None,
        CoverSource::Automatic | CoverSource::Server => custom.or(connected),
    }
}

#[derive(Default)]
struct AddressState {
    source: CoverSource,
    /// The "Public server address" setting, when it holds a usable address.
    custom: Option<Url>,
    /// The connected server's address, as the apps know it.
    server: Option<String>,
    /// What [`public_server_address`] returned for `server`, and when.
    server_checked: Option<(Option<Url>, Instant)>,
    generation: u64,
}

/// Where Discord covers come from, shared with the art thread so changes apply without restarting Discord status. Each
/// change bumps a generation, which tells the status to drop covers it already chose.
#[derive(Clone, Default)]
pub struct PublicAddress(std::sync::Arc<Mutex<AddressState>>);

impl PublicAddress {
    fn state(&self) -> std::sync::MutexGuard<'_, AddressState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Settings > Discord "Covers from".
    pub fn set_source(&self, source: CoverSource) {
        let mut state = self.state();
        if state.source != source {
            state.source = source;
            state.generation += 1;
        }
    }

    pub fn source(&self) -> CoverSource {
        self.state().source
    }

    /// The "Public server address" setting, already validated by [`parse_public_address`].
    pub fn set_custom(&self, url: Option<Url>) {
        let mut state = self.state();
        if state.custom != url {
            state.custom = url;
            state.generation += 1;
        }
    }

    /// The connected server's address as signed in, or None.
    pub fn set_server(&self, server: Option<&str>) {
        let mut state = self.state();
        if state.server.as_deref() != server {
            state.server = server.map(str::to_owned);
            state.server_checked = None;
            state.generation += 1;
        }
    }

    /// The address to use now. For `Automatic` without its own address, the server name is looked up in DNS the first
    /// time and again after a while. This blocks, so only call it from the art thread.
    pub fn get(&self) -> Option<Url> {
        let (source, custom, server, checked) = {
            let state = self.state();
            (state.source, state.custom.clone(), state.server.clone(), state.server_checked.clone())
        };
        if source == CoverSource::Search || custom.is_some() {
            return cover_address(source, custom, None);
        }
        let server = server?;
        if source == CoverSource::Server {
            return cover_address(source, None, server_address_as_is(&server));
        }
        if let Some((known, at)) = checked
            && at.elapsed() < SERVER_RETRY_AFTER
        {
            return known;
        }
        let found = public_server_address(&server);
        let mut state = self.state();
        if state.server.as_deref() == Some(server.as_str()) {
            if state.server_checked.as_ref().is_some_and(|(known, _)| *known != found) {
                state.generation += 1;
            }
            state.server_checked = Some((found.clone(), Instant::now()));
        }
        cover_address(source, None, found)
    }

    /// Whether the cover link is used without a test load. The user chose "My server", so a link this computer can't
    /// load, say behind a router without loopback, is still sent.
    fn trusted(&self) -> bool {
        self.state().source == CoverSource::Server
    }

    pub fn generation(&self) -> u64 {
        self.state().generation
    }
}

/// The server's own cover through its public address: `getAlbumInfo2`'s `largeImageUrl` rewritten by
/// [`public_cover_url`]. The first link on an address is loaded once to check that it really serves covers, and the
/// result is kept for that address.
pub struct ServerCover {
    lookup: std::sync::Arc<dyn ServerLookup>,
    address: PublicAddress,
    client: Client,
    /// Per address, whether a link loaded and when that was learned.
    checked: Mutex<HashMap<String, (bool, Instant)>>,
}

impl ServerCover {
    pub fn new(lookup: std::sync::Arc<dyn ServerLookup>, address: PublicAddress) -> Result<Self, ArtError> {
        Ok(Self { lookup, address, client: search_client()?, checked: Mutex::new(HashMap::new()) })
    }

    pub fn find(&self, track: &TrackInfo) -> Result<Option<String>, ArtError> {
        let Some(public) = self.address.get() else { return Ok(None) };
        let Some(album_id) = track.album_id.as_deref().filter(|id| !id.is_empty()) else { return Ok(None) };
        let key = public.to_string();
        let checked = self.checked.lock().unwrap_or_else(|e| e.into_inner()).get(&key).copied();
        if let Some((false, at)) = checked
            && at.elapsed() < SERVER_RETRY_AFTER
            && !self.address.trusted()
        {
            return Ok(None);
        }
        let Some(link) = self.lookup.album_image_link(album_id)? else { return Ok(None) };
        let Some(url) = public_cover_url(&link, &public) else { return Ok(None) };
        if checked.is_some_and(|(ok, _)| ok) || self.address.trusted() {
            return Ok(Some(url));
        }
        let loads = self.loads(&url);
        self.checked.lock().unwrap_or_else(|e| e.into_inner()).insert(key, (loads, Instant::now()));
        if !loads {
            log::warn!("Discord: the public server address didn't serve a cover; using public lookups");
        }
        Ok(loads.then_some(url))
    }

    /// A GET from here, like Discord's own fetch, that succeeds with an image. Navidrome's public image route doesn't
    /// take HEAD, so only the headers are read and the body is dropped.
    fn loads(&self, url: &str) -> bool {
        self.client.get(url).send().is_ok_and(|r| {
            r.status().is_success() && r.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|t| t.starts_with("image/"))
        })
    }
}

const USER_AGENT: &str = "navia-integrations/0.1 (desktop music client)";
const MIN_MATCH_SCORE: i64 = 90;
/// MusicBrainz allows at most one request per second.
const MUSICBRAINZ_INTERVAL: Duration = Duration::from_millis(1100);
const MAX_SEARCH_CANDIDATES: usize = 2;
const PROBE_RETRY_DELAY: Duration = Duration::from_millis(500);

/// Looks up the front cover in the Cover Art Archive by the track's MusicBrainz ids, or by a MusicBrainz search on
/// artist and album if there are none.
pub struct CoverArtArchive {
    client: Client,
    caa_base: String,
    musicbrainz_base: String,
    last_search: Mutex<Option<Instant>>,
}

impl CoverArtArchive {
    pub fn new() -> Result<Self, ArtError> {
        Self::with_bases("https://coverartarchive.org", "https://musicbrainz.org")
    }

    /// Uses other hosts, for testing.
    pub fn with_bases(caa_base: &str, musicbrainz_base: &str) -> Result<Self, ArtError> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .redirect(Policy::limited(5))
            .build()?;
        Ok(Self {
            client,
            caa_base: caa_base.trim_end_matches('/').to_owned(),
            musicbrainz_base: musicbrainz_base.trim_end_matches('/').to_owned(),
            last_search: Mutex::new(None),
        })
    }

    /// Checks that a cover exists and returns its final URL, so Discord gets the image itself and not a redirect chain.
    fn probe(&self, kind: &str, mbid: &str) -> Result<Option<String>, ArtError> {
        let url = format!("{}/{kind}/{mbid}/front-500", self.caa_base);
        let mut response = self.client.head(&url).send()?;
        // The archive sometimes returns 500 briefly. 503 means slow down, so that one isn't retried.
        if response.status().is_server_error() && response.status().as_u16() != 503 {
            std::thread::sleep(PROBE_RETRY_DELAY);
            response = self.client.head(&url).send()?;
        }
        let status = response.status();
        if status.is_success() {
            Ok(Some(response.url().to_string()))
        } else if status.as_u16() == 404 {
            Ok(None)
        } else {
            Err(ArtError(format!("Cover Art Archive returned {status}")))
        }
    }

    fn search_release_groups(&self, artist: &str, album: &str) -> Result<Vec<String>, ArtError> {
        self.wait_for_search_slot();
        let query = format!(
            "releasegroup:\"{}\" AND artist:\"{}\"",
            escape_lucene(album),
            escape_lucene(artist)
        );
        let url = Url::parse_with_params(
            &format!("{}/ws/2/release-group", self.musicbrainz_base),
            &[("query", query.as_str()), ("fmt", "json"), ("limit", "5")],
        )
        .map_err(|error| ArtError(error.to_string()))?;
        let response = self.client.get(url).send()?;
        let status = response.status();
        if !status.is_success() {
            return Err(ArtError(format!("MusicBrainz returned {status}")));
        }
        let body: Value = serde_json::from_slice(&response.bytes()?)
            .map_err(|error| ArtError(error.to_string()))?;
        Ok(matching_release_groups(&body, artist, album))
    }

    fn wait_for_search_slot(&self) {
        let mut last = self.last_search.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = *last {
            let wait = MUSICBRAINZ_INTERVAL.saturating_sub(previous.elapsed());
            if !wait.is_zero() {
                std::thread::sleep(wait);
            }
        }
        *last = Some(Instant::now());
    }
}

impl CoverArtArchive {
    /// Only the exact lookup by the track's tagged MusicBrainz ids. It can't pick the wrong album, so [`PublicArt`]
    /// tries it before any search.
    pub fn find_by_tags(&self, track: &TrackInfo) -> Result<Option<String>, ArtError> {
        if let Some(mbid) = track.release_mbid.as_deref().filter(|id| is_mbid(id))
            && let Some(url) = self.probe("release", mbid)?
        {
            return Ok(Some(url));
        }
        if let Some(mbid) = track.release_group_mbid.as_deref().filter(|id| is_mbid(id))
            && let Some(url) = self.probe("release-group", mbid)?
        {
            return Ok(Some(url));
        }
        Ok(None)
    }

    /// A MusicBrainz search on artist and album, then the archive's cover for the matches.
    pub fn find_by_search(&self, track: &TrackInfo) -> Result<Option<String>, ArtError> {
        if track.artist.trim().is_empty() || track.album.trim().is_empty() {
            return Ok(None);
        }
        for mbid in self
            .search_release_groups(first_artist(&track.artist), &track.album)?
            .iter()
            .take(MAX_SEARCH_CANDIDATES)
        {
            if let Some(url) = self.probe("release-group", mbid)? {
                return Ok(Some(url));
            }
        }
        Ok(None)
    }
}

impl ArtSource for CoverArtArchive {
    fn find(&self, track: &TrackInfo) -> Result<Option<String>, ArtError> {
        if let Some(url) = self.find_by_tags(track)? {
            return Ok(Some(url));
        }
        self.find_by_search(track)
    }
}

/// The art chain for Discord. With a public server address, the server's own cover comes first ([`ServerCover`]). Then
/// exact lookups by ISRC on Deezer and by tagged MusicBrainz id, then album searches on iTunes, Deezer and MusicBrainz,
/// which only accept a normalized title and artist match. An ISRC hit on a different album, like a compilation, waits
/// until the album searches miss. Last, Deezer's and iTunes' song searches ([`WantedTrack::accepts`]).
///
/// Every source is keyless, and only names, the ISRC and tagged MusicBrainz ids are sent.
pub struct PublicArt {
    caa: CoverArtArchive,
    itunes: ItunesArt,
    deezer: DeezerArt,
    server: Option<std::sync::Arc<dyn ServerLookup>>,
    cover: Option<ServerCover>,
}

impl PublicArt {
    pub fn new() -> Result<Self, ArtError> {
        Ok(Self { caa: CoverArtArchive::new()?, itunes: ItunesArt::new()?, deezer: DeezerArt::new()?, server: None, cover: None })
    }

    /// Other hosts for every source, for testing.
    pub fn with_bases(caa: &str, musicbrainz: &str, itunes: &str, deezer: &str) -> Result<Self, ArtError> {
        Ok(Self {
            caa: CoverArtArchive::with_bases(caa, musicbrainz)?,
            itunes: ItunesArt::with_base(itunes)?,
            deezer: DeezerArt::with_base(deezer)?,
            server: None,
            cover: None,
        })
    }

    /// Asks the app's server for a song's ISRC when the track has none. While `address` holds a public address, the
    /// server's own cover is tried first.
    pub fn with_server(mut self, lookup: std::sync::Arc<dyn ServerLookup>, address: PublicAddress) -> Result<Self, ArtError> {
        self.cover = Some(ServerCover::new(lookup.clone(), address)?);
        self.server = Some(lookup);
        Ok(self)
    }

    fn isrc(&self, track: &TrackInfo) -> Option<String> {
        if let Some(isrc) = track.isrc.as_deref().and_then(clean_isrc) {
            return Some(isrc);
        }
        let server = self.server.as_ref().filter(|_| !track.id.is_empty())?;
        server.song_isrc(&track.id).ok().flatten().as_deref().and_then(clean_isrc)
    }
}

impl ArtSource for PublicArt {
    fn find(&self, track: &TrackInfo) -> Result<Option<String>, ArtError> {
        Ok(self.find_art(track)?.map(|art| art.url))
    }

    fn generation(&self) -> u64 {
        self.cover.as_ref().map_or(0, |c| c.address.generation())
    }

    fn find_art(&self, track: &TrackInfo) -> Result<Option<Art>, ArtError> {
        // A failing source is skipped so the next can still answer. The error only surfaces when nothing was found, so
        // a later song can try again.
        let mut failure = None;
        let mut take = |result: Result<Option<String>, ArtError>| match result {
            Ok(found) => found,
            Err(error) => {
                failure = Some(error);
                None
            }
        };
        let album = |url| Some(Art { url, album_wide: true });
        let song = |url| Some(Art { url, album_wide: false });
        if let Some(cover) = &self.cover
            && let Some(url) = take(cover.find(track))
        {
            return Ok(album(url));
        }
        let mut isrc_elsewhere = None;
        if let Some(isrc) = self.isrc(track) {
            match self.deezer.find_isrc(&isrc, track) {
                Ok(Some(hit)) if hit.same_album => return Ok(song(hit.url)),
                Ok(Some(hit)) => isrc_elsewhere = Some(hit.url),
                Ok(None) => {}
                Err(error) => {
                    take(Err(error));
                }
            }
        }
        type Step<'a> = &'a dyn Fn() -> Result<Option<String>, ArtError>;
        let albums: [Step; 4] = [
            &|| self.caa.find_by_tags(track),
            &|| self.itunes.find(track),
            &|| self.deezer.find(track),
            &|| self.caa.find_by_search(track),
        ];
        for step in albums {
            if let Some(url) = take(step()) {
                return Ok(album(url));
            }
        }
        if let Some(url) = isrc_elsewhere {
            return Ok(song(url));
        }
        let songs: [Step; 2] = [&|| self.deezer.find_track(track), &|| self.itunes.find_track(track)];
        for step in songs {
            if let Some(url) = take(step()) {
                return Ok(song(url));
            }
        }
        failure.map_or(Ok(None), Err)
    }
}

/// Short timeouts, since a slow service only delays the cover and Discord shows the default image meanwhile.
fn search_client() -> Result<Client, ArtError> {
    Ok(Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(5))
        .redirect(Policy::limited(3))
        .build()?)
}

/// Spaces requests to one service at least `interval` apart.
struct Pacer {
    interval: Duration,
    last: Mutex<Option<Instant>>,
}

impl Pacer {
    fn new(interval: Duration) -> Self {
        Self { interval, last: Mutex::new(None) }
    }

    fn wait(&self) {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = *last {
            let wait = self.interval.saturating_sub(previous.elapsed());
            if !wait.is_zero() {
                std::thread::sleep(wait);
            }
        }
        *last = Some(Instant::now());
    }
}

fn get_json(client: &Client, url: Url, service: &str) -> Result<Value, ArtError> {
    let response = client.get(url).send()?;
    let status = response.status();
    if !status.is_success() {
        return Err(ArtError(format!("{service} returned {status}")));
    }
    serde_json::from_slice(&response.bytes()?).map_err(|error| ArtError(format!("{service}: {error}")))
}

fn searchable(track: &TrackInfo) -> bool {
    !normalize(&track.artist).is_empty() && !normalize(&track.album).is_empty()
}

/// Apple's iTunes Search API. Apple asks for at most about 20 requests a minute.
pub struct ItunesArt {
    client: Client,
    base: String,
    pacer: Pacer,
}

impl ItunesArt {
    pub fn new() -> Result<Self, ArtError> {
        Self::with_base("https://itunes.apple.com")
    }

    pub fn with_base(base: &str) -> Result<Self, ArtError> {
        Ok(Self { client: search_client()?, base: base.trim_end_matches('/').to_owned(), pacer: Pacer::new(Duration::from_millis(3100)) })
    }
}

impl ArtSource for ItunesArt {
    fn find(&self, track: &TrackInfo) -> Result<Option<String>, ArtError> {
        if !searchable(track) {
            return Ok(None);
        }
        self.pacer.wait();
        let term = format!("{} {}", first_artist(&track.artist), strip_edition(&track.album));
        let url = Url::parse_with_params(&format!("{}/search", self.base), &[("term", term.as_str()), ("entity", "album"), ("limit", "15")])
            .map_err(|error| ArtError(error.to_string()))?;
        let body = get_json(&self.client, url, "iTunes")?;
        Ok(itunes_match(&body, &track.artist, &track.album))
    }
}

impl ItunesArt {
    /// Finds the song itself in iTunes' song search when no album matched, and uses that song's album cover.
    pub fn find_track(&self, track: &TrackInfo) -> Result<Option<String>, ArtError> {
        let Some(wanted) = WantedTrack::new(track) else { return Ok(None) };
        self.pacer.wait();
        let term = wanted.search_terms();
        let url = Url::parse_with_params(&format!("{}/search", self.base), &[("term", term.as_str()), ("entity", "song"), ("limit", "25")])
            .map_err(|error| ArtError(error.to_string()))?;
        let body = get_json(&self.client, url, "iTunes")?;
        Ok(itunes_track_match(&body, &wanted))
    }
}

/// The album artwork of the best accepted song in an iTunes song search, at 600 px. Compilations are skipped.
fn itunes_track_match(body: &Value, wanted: &WantedTrack) -> Option<String> {
    let results = body["results"].as_array()?;
    let candidates = results
        .iter()
        .filter(|r| r["wrapperType"].as_str().is_none_or(|t| t == "track") && r["kind"].as_str().is_none_or(|k| k == "song"))
        .filter(|r| !normalize(r["collectionArtistName"].as_str().unwrap_or("")).contains("variousartists"))
        .filter_map(|r| Some((r["trackName"].as_str()?, r["artistName"].as_str()?, r["collectionName"].as_str().unwrap_or(""), r["artworkUrl100"].as_str()?)));
    wanted.best(candidates).and_then(|art| https_url(&art.replace("100x100bb", "600x600bb")))
}

/// Picks the matching album's artwork from an iTunes response, at 600 px.
fn itunes_match(body: &Value, artist: &str, album: &str) -> Option<String> {
    let results = body["results"].as_array()?;
    let candidates = results.iter().filter(|r| r["wrapperType"].as_str().is_none_or(|t| t == "collection")).filter_map(|r| {
        let art = r["artworkUrl100"].as_str()?;
        Some((r["collectionName"].as_str()?, r["artistName"].as_str()?, art))
    });
    best_match(candidates, artist, album).and_then(|art| {
        let big = art.replace("100x100bb", "600x600bb");
        https_url(&big)
    })
}

/// Deezer's public search. Deezer allows 50 requests per 5 seconds, and this stays far below that.
pub struct DeezerArt {
    client: Client,
    base: String,
    pacer: Pacer,
}

impl DeezerArt {
    pub fn new() -> Result<Self, ArtError> {
        Self::with_base("https://api.deezer.com")
    }

    pub fn with_base(base: &str) -> Result<Self, ArtError> {
        Ok(Self { client: search_client()?, base: base.trim_end_matches('/').to_owned(), pacer: Pacer::new(Duration::from_millis(500)) })
    }
}

impl ArtSource for DeezerArt {
    fn find(&self, track: &TrackInfo) -> Result<Option<String>, ArtError> {
        if !searchable(track) {
            return Ok(None);
        }
        self.pacer.wait();
        let q = format!("artist:\"{}\" album:\"{}\"", escape_lucene(first_artist(&track.artist)), escape_lucene(&strip_edition(&track.album)));
        let url = Url::parse_with_params(&format!("{}/search/album", self.base), &[("q", q.as_str()), ("limit", "15")])
            .map_err(|error| ArtError(error.to_string()))?;
        let body = get_json(&self.client, url, "Deezer")?;
        if body.get("error").is_some() {
            return Err(ArtError("Deezer returned an error".into()));
        }
        Ok(deezer_match(&body, &track.artist, &track.album))
    }
}

impl DeezerArt {
    /// Finds the song itself in Deezer's track search when no album matched, and uses that song's album cover.
    pub fn find_track(&self, track: &TrackInfo) -> Result<Option<String>, ArtError> {
        let Some(wanted) = WantedTrack::new(track) else { return Ok(None) };
        self.pacer.wait();
        let q = wanted.search_terms();
        let url = Url::parse_with_params(&format!("{}/search/track", self.base), &[("q", q.as_str()), ("limit", "25")])
            .map_err(|error| ArtError(error.to_string()))?;
        let body = get_json(&self.client, url, "Deezer")?;
        if body.get("error").is_some() {
            return Err(ArtError("Deezer returned an error".into()));
        }
        Ok(deezer_track_match(&body, &wanted))
    }
}

/// A song found by its ISRC.
#[derive(Debug, PartialEq, Eq)]
pub struct IsrcHit {
    pub url: String,
    /// The store files it under an album with our album's title, ignoring editions.
    pub same_album: bool,
}

impl DeezerArt {
    /// The exact recording by ISRC (`/track/isrc:<ISRC>`). Only accepted when Deezer's main artist or one of its
    /// contributors is our first artist.
    pub fn find_isrc(&self, isrc: &str, track: &TrackInfo) -> Result<Option<IsrcHit>, ArtError> {
        let Some(isrc) = clean_isrc(isrc) else { return Ok(None) };
        self.pacer.wait();
        let url = Url::parse(&format!("{}/track/isrc:{isrc}", self.base)).map_err(|error| ArtError(error.to_string()))?;
        let body = get_json(&self.client, url, "Deezer")?;
        // "no data" (code 800) is a clean miss. Anything else is a failure worth retrying later.
        if let Some(error) = body.get("error") {
            return if error["code"].as_i64() == Some(800) { Ok(None) } else { Err(ArtError("Deezer returned an error".into())) };
        }
        Ok(deezer_isrc_match(&body, track))
    }
}

fn deezer_isrc_match(body: &Value, track: &TrackInfo) -> Option<IsrcHit> {
    let first = normalize(first_artist(&track.artist));
    let main = body["artist"]["name"].as_str().unwrap_or("");
    let contributor = || body["contributors"].as_array().is_some_and(|all| all.iter().any(|c| c["name"].as_str().is_some_and(|n| !first.is_empty() && normalize(n) == first)));
    if !artists_match(&track.artist, main) && !contributor() {
        return None;
    }
    let album = &body["album"];
    let url = https_url(album["cover_xl"].as_str().or_else(|| album["cover_big"].as_str())?)?;
    let theirs = normalize(&strip_edition(album["title"].as_str().unwrap_or("")));
    let same_album = !theirs.is_empty() && theirs == normalize(&strip_edition(&track.album));
    Some(IsrcHit { url, same_album })
}

/// An ISRC as the stores take it: 12 uppercase letters and digits, without dashes or spaces.
fn clean_isrc(text: &str) -> Option<String> {
    let isrc: String = text.chars().filter(|c| !matches!(c, '-' | ' ')).map(|c| c.to_ascii_uppercase()).collect();
    (isrc.len() == 12 && isrc.bytes().all(|b| b.is_ascii_alphanumeric())).then_some(isrc)
}

fn deezer_track_match(body: &Value, wanted: &WantedTrack) -> Option<String> {
    let data = body["data"].as_array()?;
    let candidates = data.iter().filter_map(|t| {
        let album = &t["album"];
        let art = album["cover_xl"].as_str().or_else(|| album["cover_big"].as_str())?;
        Some((t["title"].as_str()?, t["artist"]["name"].as_str()?, album["title"].as_str().unwrap_or(""), art))
    });
    wanted.best(candidates).and_then(https_url)
}

fn deezer_match(body: &Value, artist: &str, album: &str) -> Option<String> {
    let data = body["data"].as_array()?;
    let candidates = data.iter().filter_map(|a| {
        let art = a["cover_xl"].as_str().or_else(|| a["cover_big"].as_str())?;
        Some((a["title"].as_str()?, a["artist"]["name"].as_str()?, art))
    });
    best_match(candidates, artist, album).and_then(https_url)
}

/// Only `https` URLs go to Discord.
fn https_url(url: &str) -> Option<String> {
    Url::parse(url).ok().filter(|u| u.scheme() == "https" && u.host_str().is_some()).map(|u| u.to_string())
}

/// The artwork of the first candidate whose album and artist match. An exact normalized title wins over one that only
/// matches after edition words like "(Deluxe)" or " - Single" are stripped.
fn best_match<'a>(candidates: impl Iterator<Item = (&'a str, &'a str, &'a str)>, artist: &str, album: &str) -> Option<&'a str> {
    let exact = normalize(album);
    let loose = normalize(&strip_edition(album));
    if loose.is_empty() || normalize(artist).is_empty() {
        return None;
    }
    let mut fallback = None;
    for (title, by, art) in candidates {
        if !artists_match(artist, by) {
            continue;
        }
        if normalize(title) == exact {
            return Some(art);
        }
        if fallback.is_none() && normalize(&strip_edition(title)) == loose {
            fallback = Some(art);
        }
    }
    fallback
}

/// Words that mark an edition of the same album. "Live", "Acoustic" and "Instrumental" are different recordings, so
/// they aren't here.
const EDITION_WORDS: &[&str] = &[
    "deluxe", "edition", "remaster", "remastered", "expanded", "anniversary", "bonus", "special", "explicit", "clean",
    "reissue", "version", "collector", "collectors", "super", "limited", "standard", "mono", "stereo", "tracks", "track",
    // As in "Apple Digital Master" or "2011 Master".
    "master",
];

/// A format note in a tagged album title, like "24bit" or "96khz".
fn is_format_word(word: &str) -> bool {
    let unit = word.trim_end_matches("khz").trim_end_matches("bit");
    unit.len() < word.len() && !unit.is_empty() && unit.bytes().all(|b| b.is_ascii_digit())
}

/// Strips trailing bracketed edition notes and iTunes' " - Single" or " - EP" suffixes.
fn strip_edition(title: &str) -> String {
    let mut t = title.trim().to_owned();
    loop {
        let before = t.clone();
        for suffix in [" - Single", " - EP"] {
            if t.len() > suffix.len() && t.to_lowercase().ends_with(&suffix.to_lowercase()) {
                t.truncate(t.len() - suffix.len());
                t = t.trim_end().to_owned();
            }
        }
        if let Some(close) = t.chars().last().filter(|c| matches!(c, ')' | ']'))
            && let Some(open_at) = t.rfind(if close == ')' { '(' } else { '[' })
            && open_at > 0
        {
            let inner = t[open_at + 1..t.len() - 1].to_lowercase();
            let is_edition = inner
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| !w.is_empty())
                .any(|w| EDITION_WORDS.contains(&w) || is_format_word(w) || w.chars().all(|c| c.is_ascii_digit()) && w.len() == 4);
            if is_edition {
                t.truncate(open_at);
                t = t.trim_end().trim_end_matches(['-', ':']).trim_end().to_owned();
            }
        }
        if t == before {
            return t;
        }
    }
}

/// The same artist once normalized, or the same first credited artist, so "A feat. B" still matches "A".
fn artists_match(wanted: &str, found: &str) -> bool {
    let (w, f) = (normalize(wanted), normalize(found));
    if w.is_empty() || f.is_empty() {
        return false;
    }
    w == f || normalize(first_artist(wanted)) == normalize(first_artist(found)) && !normalize(first_artist(wanted)).is_empty()
}

fn first_artist(credit: &str) -> &str {
    let lower = credit.to_lowercase();
    let mut cut = credit.len();
    // " • " is Navidrome's default artist joiner (`Scanner.ArtistJoiner`), so multi-artist songs arrive that way.
    for sep in [" • ", " · ", " feat. ", " feat ", " ft. ", " featuring ", " & ", " and ", " x ", ", ", "; ", " / "] {
        if let Some(at) = lower.find(sep) {
            cut = cut.min(at);
        }
    }
    // Lowercasing can change byte lengths in some scripts. Fall back to the whole credit then.
    credit.get(..cut).unwrap_or(credit)
}

/// Words in a title's decoration that make it a different recording, so both sides must agree on them.
const RECORDING_MARKERS: &[&str] = &["instrumental", "live", "karaoke", "acoustic", "acapella", "cappella"];

/// Words that mark a remix or version decoration. The rest of it, like the "X" in "X Remix", names an artist.
const VERSION_WORDS: &[&str] = &[
    "remix", "remixed", "mix", "rework", "edit", "dub", "bootleg", "flip", "vip", "radio", "extended", "original", "club",
    "single", "album", "short", "long", "dirty", "cut", "rerecorded", "recorded", "re", "by", "the", "from",
];

/// Markers of someone else's recording of the song, unless ours has them too.
const COVER_WORDS: &[&str] = &["karaoke", "tribute", "originally performed", "in the style of", "made famous", "as performed by"];

/// Lowercase prefixes of a credit decoration, as in "(feat. X)", "(with X)" or " - ft. X".
const CREDIT_PREFIXES: &[&str] = &["feat. ", "feat ", "ft. ", "ft ", "featuring ", "with "];

/// A song title split into its plain title, the artists its decorations name, and the words that make it a different
/// recording.
#[derive(Debug, Default, PartialEq)]
struct TitleParts {
    /// The plain title, as written.
    core: String,
    /// Artists named in the decorations, as written.
    names: Vec<String>,
    markers: Vec<&'static str>,
}

/// What one decoration says, or None if it's part of the title, like "(Part 2)".
fn decoration(inner: &str) -> Option<(Vec<String>, Vec<&'static str>)> {
    let inner = inner.trim();
    let lower = inner.to_ascii_lowercase();
    for prefix in CREDIT_PREFIXES {
        if lower.starts_with(prefix) {
            return Some((split_names(&inner[prefix.len()..]), Vec::new()));
        }
    }
    let words: Vec<(&str, String)> = inner.split_whitespace().map(|w| (w, normalize(w))).filter(|(_, n)| !n.is_empty()).collect();
    let markers: Vec<&'static str> = RECORDING_MARKERS.iter().copied().filter(|m| words.iter().any(|(_, n)| n == m)).collect();
    let is_year = |n: &str| n.len() == 4 && n.chars().all(|c| c.is_ascii_digit());
    let is_version = |n: &str| VERSION_WORDS.contains(&n) || EDITION_WORDS.contains(&n);
    if !markers.is_empty() {
        return Some((Vec::new(), markers));
    }
    if !words.iter().any(|(_, n)| is_version(n) && !matches!(n.as_str(), "by" | "the" | "from" | "re")) {
        return None;
    }
    let rest: Vec<&str> = words.iter().filter(|(_, n)| !is_version(n) && !is_year(n)).map(|(w, _)| *w).collect();
    let names = if rest.is_empty() { Vec::new() } else { split_names(&rest.join(" ")) };
    Some((names, Vec::new()))
}

impl TitleParts {
    fn parse(title: &str) -> Self {
        let mut parts = TitleParts::default();
        let mut t = title.trim().to_owned();
        loop {
            let before = t.len();
            // A trailing bracket.
            if let Some(close) = t.chars().last().filter(|c| matches!(c, ')' | ']'))
                && let Some(open_at) = t.rfind(if close == ')' { '(' } else { '[' })
                && open_at > 0
                && let Some((names, markers)) = decoration(&t[open_at + 1..t.len() - 1])
            {
                parts.add(names, markers);
                t.truncate(open_at);
            } else if let Some(at) = t.rfind(" - ")
                && at > 0
                && let Some((names, markers)) = decoration(&t[at + 3..])
            {
                parts.add(names, markers);
                t.truncate(at);
            }
            t = t.trim_end().to_owned();
            if t.len() == before {
                break;
            }
        }
        // An unbracketed "Song feat. X" or "Song ft. X".
        let lower = t.to_ascii_lowercase();
        if let Some(at) = [" feat. ", " feat ", " ft. ", " featuring "].iter().filter_map(|p| lower.find(p).map(|at| (at, p.len()))).min()
            .filter(|(at, _)| *at > 0)
        {
            parts.add(split_names(&t[at.0 + at.1..]), Vec::new());
            t.truncate(at.0);
        }
        parts.core = t.trim().to_owned();
        parts.markers.sort_unstable();
        parts.markers.dedup();
        parts
    }

    fn add(&mut self, names: Vec<String>, markers: Vec<&'static str>) {
        self.names.extend(names);
        self.markers.extend(markers);
    }
}

/// Splits a credit like "A, B & C" into names.
fn split_names(credit: &str) -> Vec<String> {
    let mut parts = vec![credit.to_owned()];
    for sep in [" • ", " · ", " feat. ", " feat ", " ft. ", " featuring ", " & ", " and ", " x ", " + ", ", ", "; ", " / "] {
        parts = parts
            .into_iter()
            .flat_map(|part| {
                let lower = part.to_ascii_lowercase();
                let mut out = Vec::new();
                let mut start = 0;
                for (at, _) in lower.match_indices(sep) {
                    out.push(part[start..at].to_owned());
                    start = at + sep.len();
                }
                out.push(part[start..].to_owned());
                out
            })
            .collect();
    }
    parts.into_iter().map(|p| p.trim().to_owned()).filter(|p| !normalize(p).is_empty()).collect()
}

/// The playing song, ready to compare with a store's song search.
#[derive(Debug)]
struct WantedTrack {
    artist: String,
    first_artist: String,
    parts: TitleParts,
    /// Normalized names besides the first artist that a result must show: the other credited artists and those in the
    /// title's decorations.
    required: Vec<String>,
    /// The same names as written, for the search.
    required_text: Vec<String>,
    /// Our title, artist and album, normalized. A result's own decoration names must appear here.
    ours: String,
    /// Our title, artist and album, lowercase, for [`COVER_WORDS`].
    ours_lower: String,
}

impl WantedTrack {
    fn new(track: &TrackInfo) -> Option<Self> {
        let first = first_artist(&track.artist).trim().to_owned();
        let parts = TitleParts::parse(&track.title);
        if normalize(&first).is_empty() || normalize(&parts.core).is_empty() {
            return None;
        }
        let first_key = normalize(&first);
        let mut required = Vec::new();
        let mut required_text = Vec::new();
        for name in split_names(&track.artist).into_iter().chain(parts.names.iter().cloned()) {
            let key = normalize(&name);
            if key != first_key && !required.contains(&key) {
                required.push(key);
                required_text.push(name);
            }
        }
        let ours = format!("{}{}{}", normalize(&track.title), normalize(&track.artist), normalize(&track.album));
        let ours_lower = format!("{} {} {}", track.title, track.artist, track.album).to_lowercase();
        Some(Self { artist: track.artist.clone(), first_artist: first, parts, required, required_text, ours, ours_lower })
    }

    /// The first artist, the plain title and the other artists' names.
    fn search_terms(&self) -> String {
        let mut terms = vec![self.first_artist.as_str(), self.parts.core.as_str()];
        terms.extend(self.required_text.iter().map(String::as_str));
        terms.join(" ")
    }

    /// Whether a store's song is this recording. Strict, since a miss is better than a wrong cover.
    fn accepts(&self, title: &str, artist: &str, album: &str) -> bool {
        if !artists_match(&self.artist, artist) {
            return false;
        }
        let theirs_lower = format!("{title} {artist} {album}").to_lowercase();
        if COVER_WORDS.iter().any(|w| theirs_lower.contains(w) && !self.ours_lower.contains(w)) {
            return false;
        }
        let parts = TitleParts::parse(title);
        if normalize(&parts.core) != normalize(&self.parts.core) || parts.markers != self.parts.markers {
            return false;
        }
        // Everyone we credit must show on their side, so a plain original doesn't stand in for a featured version.
        let shown = format!("{}{}", normalize(title), normalize(artist));
        if !self.required.iter().all(|name| shown.contains(name.as_str())) {
            return false;
        }
        // And everyone their decorations name must be ours, so another remixer's version doesn't either.
        parts.names.iter().map(|n| normalize(n)).all(|name| self.ours.contains(&name))
    }

    /// The art of the accepted candidate, preferring one whose album is named after the song (its single), then the
    /// first.
    fn best<'a>(&self, candidates: impl Iterator<Item = (&'a str, &'a str, &'a str, &'a str)>) -> Option<&'a str> {
        let core = normalize(&self.parts.core);
        let mut first = None;
        for (title, artist, album, art) in candidates {
            if !self.accepts(title, artist, album) {
                continue;
            }
            if normalize(&TitleParts::parse(&strip_edition(album)).core) == core {
                return Some(art);
            }
            first.get_or_insert(art);
        }
        first
    }
}

fn is_mbid(id: &str) -> bool {
    id.len() == 36
        && id.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn escape_lucene(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Picks release groups from a search response that really are this album. A wrong cover on a public profile is worse
/// than the default image, so the normalized album title must match exactly and the artist must appear in the credit.
/// Albums sort before singles and other types.
fn matching_release_groups(body: &Value, artist: &str, album: &str) -> Vec<String> {
    let wanted_album = normalize(album);
    let wanted_artist = normalize(artist);
    if wanted_album.is_empty() || wanted_artist.is_empty() {
        return Vec::new();
    }
    let Some(groups) = body["release-groups"].as_array() else {
        return Vec::new();
    };
    let mut matches: Vec<(bool, String)> = groups
        .iter()
        .filter(|group| group["score"].as_i64().unwrap_or(0) >= MIN_MATCH_SCORE)
        .filter(|group| normalize(group["title"].as_str().unwrap_or("")) == wanted_album)
        .filter(|group| credit_matches(group, &wanted_artist))
        .filter_map(|group| {
            let id = group["id"].as_str()?;
            let is_album = group["primary-type"].as_str() == Some("Album");
            is_mbid(id).then(|| (!is_album, id.to_owned()))
        })
        .collect();
    matches.sort_by_key(|(not_album, _)| *not_album);
    matches.into_iter().map(|(_, id)| id).collect()
}

fn credit_matches(group: &Value, wanted_artist: &str) -> bool {
    group["artist-credit"].as_array().is_some_and(|credits| {
        credits.iter().any(|credit| {
            let name = normalize(credit["name"].as_str().unwrap_or(""));
            !name.is_empty() && (wanted_artist.contains(&name) || name.contains(wanted_artist))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID_A: &str = "aaaaaaaa-1111-2222-3333-444444444444";
    const ID_B: &str = "bbbbbbbb-1111-2222-3333-444444444444";

    fn group(id: &str, score: i64, title: &str, artist: &str, kind: &str) -> Value {
        json!({
            "id": id, "score": score, "title": title, "primary-type": kind,
            "artist-credit": [{"name": artist, "artist": {"name": artist}}],
        })
    }

    #[test]
    fn accepts_only_matching_title_and_artist() {
        let body = json!({"release-groups": [
            group(ID_A, 100, "Abbey Road", "The Beatles", "Album"),
            group(ID_B, 100, "Abbey Road Sessions", "The Beatles", "Album"),
        ]});
        assert_eq!(matching_release_groups(&body, "The Beatles", "abbey road!"), vec![ID_A]);
        assert!(matching_release_groups(&body, "Someone Else", "Abbey Road").is_empty());
    }

    #[test]
    fn rejects_low_scores_and_bad_ids() {
        let body = json!({"release-groups": [
            group(ID_A, 40, "Abbey Road", "The Beatles", "Album"),
            group("../../evil", 100, "Abbey Road", "The Beatles", "Album"),
        ]});
        assert!(matching_release_groups(&body, "The Beatles", "Abbey Road").is_empty());
    }

    #[test]
    fn albums_come_before_other_types() {
        let body = json!({"release-groups": [
            group(ID_B, 100, "Same", "Artist", "Single"),
            group(ID_A, 95, "Same", "Artist", "Album"),
        ]});
        assert_eq!(matching_release_groups(&body, "Artist", "Same"), vec![ID_A, ID_B]);
    }

    #[test]
    fn non_latin_titles_can_match() {
        let body = json!({"release-groups": [group(ID_A, 100, "الأطلال", "أم كلثوم", "Album")]});
        assert_eq!(matching_release_groups(&body, "أم كلثوم", "الأطلال"), vec![ID_A]);
    }

    #[test]
    fn edition_suffixes_come_off() {
        assert_eq!(strip_edition("Abbey Road (Remastered 2019)"), "Abbey Road");
        assert_eq!(strip_edition("Folklore (Deluxe Version)"), "Folklore");
        assert_eq!(strip_edition("Hit Me Hard and Soft - Single"), "Hit Me Hard and Soft");
        assert_eq!(strip_edition("Rumours [Super Deluxe] (2013 Remaster)"), "Rumours");
        assert_eq!(strip_edition("Unplugged (Live)"), "Unplugged (Live)");
        assert_eq!(strip_edition("(What's the Story) Morning Glory?"), "(What's the Story) Morning Glory?");
        assert_eq!(strip_edition("1989 (Taylor's Version)"), "1989");
        assert_eq!(strip_edition("Random Access Memories (apple digital master)"), "Random Access Memories");
        assert_eq!(strip_edition("Starboy (24bit/44.1kHz)"), "Starboy");
        assert_eq!(strip_edition("Songs in A Minor (Bit)"), "Songs in A Minor (Bit)");
    }

    fn itunes(items: &[(&str, &str, &str)]) -> Value {
        json!({"resultCount": items.len(), "results": items.iter().map(|(t, a, art)| json!({
            "wrapperType": "collection", "collectionName": t, "artistName": a, "artworkUrl100": art})).collect::<Vec<_>>()})
    }

    #[test]
    fn navidrome_joined_artists_match_the_first_artist() {
        assert!(artists_match("The Weeknd • Daft Punk", "The Weeknd"));
        assert!(artists_match("JAŸ-Z · Alicia Keys", "JAŸ-Z"));
        assert!(!artists_match("Daft Punk • The Weeknd", "The Weeknd"));
    }

    #[test]
    fn itunes_prefers_exact_titles_and_rejects_other_artists() {
        let body = itunes(&[
            ("Folklore (Deluxe Version)", "Taylor Swift", "https://is1.example/a/100x100bb.jpg"),
            ("folklore", "Taylor Swift", "https://is1.example/b/100x100bb.jpg"),
            ("Folklore", "Someone Else", "https://is1.example/c/100x100bb.jpg"),
        ]);
        assert_eq!(itunes_match(&body, "Taylor Swift", "Folklore").as_deref(), Some("https://is1.example/b/600x600bb.jpg"));
        assert_eq!(itunes_match(&body, "Taylor Swift", "folklore (deluxe version)").as_deref(), Some("https://is1.example/a/600x600bb.jpg"));
        assert_eq!(itunes_match(&body, "Phoebe Bridgers", "Folklore"), None);
        assert_eq!(itunes_match(&body, "Taylor Swift", "Evermore"), None);
    }

    #[test]
    fn itunes_matches_an_edition_when_there_is_no_exact_title() {
        let body = itunes(&[("Abbey Road (Remastered)", "The Beatles", "https://is1.example/x/100x100bb.jpg")]);
        assert!(itunes_match(&body, "The Beatles", "Abbey Road").is_some());
        assert!(itunes_match(&body, "The Beatles", "Abbey Road Sessions").is_none());
    }

    #[test]
    fn plain_http_art_is_refused() {
        let body = itunes(&[("A", "B", "http://is1.example/x/100x100bb.jpg")]);
        assert_eq!(itunes_match(&body, "B", "A"), None);
    }

    #[test]
    fn deezer_matches_featured_credits_by_first_artist() {
        let body = json!({"data": [
            {"title": "Same Album", "artist": {"name": "Main Artist"}, "cover_xl": "https://e-cdns.example/xl.jpg"},
        ]});
        assert!(deezer_match(&body, "Main Artist feat. Guest", "Same Album").is_some());
        assert!(deezer_match(&body, "Guest", "Same Album").is_none());
        assert!(deezer_match(&json!({"data": []}), "Main Artist", "Same Album").is_none());
    }

    /// Serves canned answers by path prefix on a local port, counting requests.
    fn fake(routes: Vec<(&'static str, u16, String)>) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut line = String::new();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                reader.read_line(&mut line).unwrap();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap() <= 2 { break }
                }
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let path = line.split_whitespace().nth(1).unwrap_or("");
                let (status, body) = routes.iter().find(|(p, _, _)| path.starts_with(p)).map_or((404, String::new()), |(_, s, b)| (*s, b.clone()));
                let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        (base, hits)
    }

    fn track(artist: &str, album: &str) -> TrackInfo {
        TrackInfo { artist: artist.into(), album: album.into(), ..Default::default() }
    }

    #[test]
    fn chain_falls_through_to_deezer_and_survives_a_failing_source() {
        let (itunes_base, _) = fake(vec![("/search", 500, String::new())]);
        let deezer_body = json!({"data": [{"title": "Album", "artist": {"name": "Artist"}, "cover_xl": "https://cdn.example/a.jpg"}]}).to_string();
        let (deezer_base, deezer_hits) = fake(vec![("/search/album", 200, deezer_body)]);
        let (mb, mb_hits) = fake(vec![]);
        let art = PublicArt::with_bases(&mb, &mb, &itunes_base, &deezer_base).unwrap();
        assert_eq!(art.find(&track("Artist", "Album")).unwrap().as_deref(), Some("https://cdn.example/a.jpg"));
        assert_eq!(deezer_hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(mb_hits.load(std::sync::atomic::Ordering::SeqCst), 0, "no MusicBrainz search once found");
    }

    #[test]
    fn chain_reports_a_clean_miss_as_none() {
        let empty = json!({"results": []}).to_string();
        let (itunes_base, _) = fake(vec![("/search", 200, empty)]);
        let (deezer_base, _) = fake(vec![("/search/album", 200, json!({"data": []}).to_string())]);
        let (mb, _) = fake(vec![("/ws/2/release-group", 200, json!({"release-groups": []}).to_string())]);
        let art = PublicArt::with_bases(&mb, &mb, &itunes_base, &deezer_base).unwrap();
        assert_eq!(art.find(&track("Nobody", "Nothing")).unwrap(), None);
    }

    fn song(artist: &str, album: &str, title: &str) -> TrackInfo {
        TrackInfo { artist: artist.into(), album: album.into(), title: title.into(), ..Default::default() }
    }

    fn wanted(artist: &str, title: &str) -> WantedTrack {
        WantedTrack::new(&song(artist, "Whatever", title)).unwrap()
    }

    fn deezer_tracks(items: &[(&str, &str, &str, &str)]) -> Value {
        json!({"data": items.iter().map(|(t, a, album, art)| json!({
            "title": t, "artist": {"name": a}, "album": {"title": album, "cover_xl": art}})).collect::<Vec<_>>()})
    }

    #[test]
    fn titles_lose_feature_and_version_decorations() {
        let p = TitleParts::parse("Dracula - JENNIE Remix");
        assert_eq!((p.core.as_str(), p.names.clone(), p.markers.is_empty()), ("Dracula", vec!["JENNIE".to_owned()], true));
        assert_eq!(TitleParts::parse("Dracula (with JENNIE)").names, vec!["JENNIE"]);
        assert_eq!(TitleParts::parse("Starboy (feat. Daft Punk) [Kygo Remix]").names, vec!["Kygo", "Daft Punk"]);
        assert_eq!(TitleParts::parse("Song ft. A & B").names, vec!["A", "B"]);
        assert_eq!(TitleParts::parse("Song (Radio Edit)").core, "Song");
        assert_eq!(TitleParts::parse("Song - Remastered 2011").core, "Song");
        assert!(TitleParts::parse("Song (Radio Edit)").names.is_empty());
        assert_eq!(TitleParts::parse("Song (Part 2)").core, "Song (Part 2)");
        assert_eq!(TitleParts::parse("Song (Live at Wembley)").markers, vec!["live"]);
        assert_eq!(TitleParts::parse("Song - Acoustic").markers, vec!["acoustic"]);
        assert_eq!(TitleParts::parse("Live Forever").core, "Live Forever");
    }

    /// The server tags a remix's album "<Title> (Remix)", while the stores only have the single.
    #[test]
    fn jennie_remix_finds_the_with_jennie_single() {
        let w = WantedTrack::new(&song("Tame Impala • JENNIE", "Dracula (Remix)", "Dracula - JENNIE Remix")).unwrap();
        assert_eq!(w.search_terms(), "Tame Impala Dracula JENNIE");
        let body = deezer_tracks(&[
            ("Dracula (with JENNIE)", "Tame Impala", "Dracula (with JENNIE)", "https://cdn.example/with.jpg"),
            ("Dracula (with JENNIE) (Instrumental)", "Tame Impala", "Dracula (with JENNIE) + Instrumental", "https://cdn.example/inst.jpg"),
            ("Dracula", "Tame Impala", "Dracula (with JENNIE)", "https://cdn.example/plain.jpg"),
        ]);
        assert_eq!(deezer_track_match(&body, &w).as_deref(), Some("https://cdn.example/with.jpg"));
    }

    #[test]
    fn karaoke_and_tribute_results_are_rejected() {
        let w = wanted("Tame Impala • JENNIE", "Dracula - JENNIE Remix");
        let body = deezer_tracks(&[
            ("Dracula (JENNIE Remix) (By Tame Impala, JENNIE) (Instrumental Karaoke Version)", "ZZang KARAOKE", "ZZang KARAOKE 2026 POP Vol.279", "https://cdn.example/k1.jpg"),
            ("Dracula (with JENNIE) (Karaoke)", "Tame Impala", "Dracula (Karaoke)", "https://cdn.example/k2.jpg"),
            ("Dracula (with JENNIE)", "Tame Impala", "Tribute to Tame Impala", "https://cdn.example/k3.jpg"),
        ]);
        assert_eq!(deezer_track_match(&body, &w), None);
        let w = wanted("The Weeknd • Daft Punk", "Starboy (feat. Daft Punk)");
        assert!(!w.accepts("Starboy (Karaoke) [Originally Performed by The Weeknd & Daft Punk]", "The Weeknd", "x"));
        assert!(!w.accepts("Starboy (feat. Daft Punk)", "Ebony Day", "Starboy (Originally Performed By The Weeknd feat. Daft Punk)"));
    }

    #[test]
    fn another_song_by_the_same_artist_is_rejected() {
        let w = wanted("Tame Impala • JENNIE", "Dracula - JENNIE Remix");
        assert!(!w.accepts("Borderline (with JENNIE)", "Tame Impala", "Borderline"));
        assert!(!w.accepts("Draculas (with JENNIE)", "Tame Impala", "x"));
        assert!(!w.accepts("Dracula (with JENNIE)", "Someone Else", "x"));
    }

    #[test]
    fn the_plain_original_does_not_stand_in_for_a_feature() {
        let w = wanted("Tame Impala", "Dracula (with JENNIE)");
        let body = deezer_tracks(&[
            ("Dracula", "Tame Impala", "Dracula - Single", "https://cdn.example/plain.jpg"),
            ("Dracula (with JENNIE)", "Tame Impala", "Dracula (with JENNIE)", "https://cdn.example/with.jpg"),
        ]);
        assert_eq!(deezer_track_match(&body, &w).as_deref(), Some("https://cdn.example/with.jpg"));
        let only_plain = deezer_tracks(&[("Dracula", "Tame Impala", "Dracula - Single", "https://cdn.example/plain.jpg")]);
        assert_eq!(deezer_track_match(&only_plain, &w), None);
        // A credit without the guest in the title still requires the guest.
        assert!(!wanted("Tame Impala • JENNIE", "Dracula").accepts("Dracula", "Tame Impala", "Dracula - Single"));
        // The other way around, someone else's remix isn't ours.
        assert!(!wanted("The Weeknd", "Starboy (feat. Daft Punk)").accepts("Starboy (feat. Daft Punk) [Kygo Remix]", "The Weeknd", "x"));
    }

    #[test]
    fn different_recordings_stay_apart() {
        let w = wanted("Artist", "Song");
        assert!(w.accepts("Song", "Artist", "Album"));
        assert!(w.accepts("Song (Radio Edit)", "Artist", "Album"));
        for other in ["Song (Instrumental)", "Song - Live", "Song (Acoustic)", "Song (Karaoke Version)"] {
            assert!(!w.accepts(other, "Artist", "Album"), "{other}");
        }
        assert!(wanted("Artist", "Song (Live)").accepts("Song - Live at Somewhere", "Artist", "Live Album"));
    }

    #[test]
    fn itunes_song_search_prefers_the_single_and_skips_compilations() {
        let w = wanted("The Weeknd • Daft Punk", "Starboy (feat. Daft Punk)");
        let r = |t: &str, c: &str, ca: Option<&str>, art: &str| json!({"wrapperType": "track", "kind": "song", "trackName": t,
            "artistName": "The Weeknd", "collectionName": c, "collectionArtistName": ca, "artworkUrl100": art});
        let body = json!({"results": [
            r("Starboy (feat. Daft Punk) [Kygo Remix]", "Starboy (Deluxe)", None, "https://is1.example/kygo/100x100bb.jpg"),
            r("Starboy (feat. Daft Punk)", "Now Hits", Some("Various Artists"), "https://is1.example/va/100x100bb.jpg"),
            r("Starboy (feat. Daft Punk)", "The Highlights", None, "https://is1.example/hl/100x100bb.jpg"),
            r("Starboy (feat. Daft Punk)", "Starboy", None, "https://is1.example/sb/100x100bb.jpg"),
        ]});
        assert_eq!(itunes_track_match(&body, &w).as_deref(), Some("https://is1.example/sb/600x600bb.jpg"));
    }

    #[test]
    fn chain_falls_back_to_the_song_search_for_this_song_only() {
        let (itunes_base, _) = fake(vec![("/search", 200, json!({"results": []}).to_string())]);
        let tracks = deezer_tracks(&[("Dracula (with JENNIE)", "Tame Impala", "Dracula (with JENNIE)", "https://cdn.example/with.jpg")]).to_string();
        let (deezer_base, _) = fake(vec![("/search/album", 200, json!({"data": []}).to_string()), ("/search/track", 200, tracks)]);
        let (mb, _) = fake(vec![("/ws/2/release-group", 200, json!({"release-groups": []}).to_string())]);
        let art = PublicArt::with_bases(&mb, &mb, &itunes_base, &deezer_base).unwrap();
        let found = art.find_art(&song("Tame Impala • JENNIE", "Dracula (Remix)", "Dracula - JENNIE Remix")).unwrap();
        assert_eq!(found, Some(Art { url: "https://cdn.example/with.jpg".into(), album_wide: false }));
    }

    const LINK: &str = "http://localhost:4533/share/img/eyJhbGciOiJIUzI1NiJ9.eyJpZCI6ImFsLTEifQ.c2ln?size=1200";

    fn address(text: &str) -> Url {
        parse_public_address(text).unwrap().unwrap()
    }

    #[test]
    fn public_address_must_be_https_without_extras() {
        assert_eq!(parse_public_address("  ").unwrap(), None);
        assert_eq!(parse_public_address("http://music.example.org"), Err(PublicAddressError::NotHttps));
        assert_eq!(parse_public_address("music.example.org"), Err(PublicAddressError::NotAUrl));
        assert_eq!(parse_public_address("https://user:pw@music.example.org"), Err(PublicAddressError::ExtraParts));
        assert_eq!(parse_public_address("https://music.example.org/?u=me"), Err(PublicAddressError::ExtraParts));
        assert!(parse_public_address(" https://music.example.org/nd/ ").unwrap().is_some());
    }

    #[test]
    fn cover_link_moves_to_the_public_address() {
        let token = "eyJhbGciOiJIUzI1NiJ9.eyJpZCI6ImFsLTEifQ.c2ln";
        assert_eq!(public_cover_url(LINK, &address("https://music.example.org")).unwrap(), format!("https://music.example.org/share/img/{token}?size=1200"));
        // The address's base path wins. Without one, the server's own base path stays.
        assert_eq!(public_cover_url(LINK, &address("https://example.org/navidrome/")).unwrap(), format!("https://example.org/navidrome/share/img/{token}?size=1200"));
        let based = "http://10.0.0.2:4533/music/share/img/eyJh.eyJp.c2ln?size=600";
        assert_eq!(public_cover_url(based, &address("https://example.org:8443")).unwrap(), "https://example.org:8443/music/share/img/eyJh.eyJp.c2ln?size=600");
    }

    #[test]
    fn cover_link_refuses_http_and_strange_links() {
        let http = Url::parse("http://music.example.org").unwrap();
        assert_eq!(public_cover_url(LINK, &http), None);
        assert_eq!(public_cover_url("http://localhost:4533/rest/getCoverArt?id=al-1&u=me&t=x&s=y", &address("https://m.example.org")), None);
        assert_eq!(public_cover_url("http://localhost:4533/share/img/../../rest", &address("https://m.example.org")), None);
        assert_eq!(public_cover_url("http://localhost:4533/share/img/a%2Fb", &address("https://m.example.org")), None);
        let long = format!("http://localhost/share/img/{}", "a".repeat(MAX_IMAGE_URL_LEN));
        assert_eq!(public_cover_url(&long, &address("https://m.example.org")), None, "too long for Discord");
    }

    #[test]
    fn cover_link_never_carries_auth_parameters() {
        let dirty = "http://admin:secret@localhost:4533/share/img/eyJh.eyJp.c2ln?u=admin&t=abc&s=salt&p=enc:7365&v=1.16.1&c=navia&jwt=x&apiKey=k&size=300";
        let url = public_cover_url(dirty, &address("https://m.example.org")).unwrap();
        assert_eq!(url, "https://m.example.org/share/img/eyJh.eyJp.c2ln?size=300");
        let parsed = Url::parse(&url).unwrap();
        assert!(parsed.username().is_empty() && parsed.password().is_none());
        for key in ["u", "t", "s", "p", "v", "c", "jwt", "apiKey"] {
            assert!(parsed.query_pairs().all(|(k, _)| k != key), "{key}");
        }
        assert_eq!(public_cover_url("http://h/share/img/eyJh?size=1e9", &address("https://m.example.org")).unwrap(), "https://m.example.org/share/img/eyJh");
    }

    fn isrc_track(artist: &str, album: &str) -> Value {
        json!({"title": "Dracula (with JENNIE)", "artist": {"name": artist}, "contributors": [{"name": artist}, {"name": "JENNIE"}],
            "album": {"title": album, "cover_xl": "https://cdn.example/isrc.jpg"}})
    }

    #[test]
    fn isrc_hits_need_the_same_artist() {
        let ours = song("Tame Impala • JENNIE", "Dracula (Remix)", "Dracula - JENNIE Remix");
        let hit = deezer_isrc_match(&isrc_track("Tame Impala", "Dracula (with JENNIE)"), &ours).unwrap();
        assert_eq!(hit, IsrcHit { url: "https://cdn.example/isrc.jpg".into(), same_album: false });
        assert!(deezer_isrc_match(&isrc_track("Tame Impala", "Dracula (Remix) [Deluxe]"), &ours).unwrap().same_album);
        // A contributor counts when it's our first artist.
        let guest_first = json!({"artist": {"name": "Someone"}, "contributors": [{"name": "Tame Impala"}], "album": {"title": "x", "cover_xl": "https://cdn.example/c.jpg"}});
        assert!(deezer_isrc_match(&guest_first, &ours).is_some());
        assert_eq!(deezer_isrc_match(&isrc_track("ZZang KARAOKE", "Hits"), &ours), None);
        let http = json!({"artist": {"name": "Tame Impala"}, "album": {"title": "x", "cover_xl": "http://cdn.example/c.jpg"}});
        assert_eq!(deezer_isrc_match(&http, &ours), None);
    }

    #[test]
    fn isrcs_are_checked_before_asking() {
        assert_eq!(clean_isrc("usqx9-26-00464").as_deref(), Some("USQX92600464"));
        assert_eq!(clean_isrc("USQX926004"), None);
        assert_eq!(clean_isrc("USQX92600464/../x"), None);
    }

    struct FakeServer {
        link: Option<String>,
        isrc: Option<String>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl ServerLookup for FakeServer {
        fn album_image_link(&self, _: &str) -> Result<Option<String>, ArtError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self.link.clone())
        }
        fn song_isrc(&self, _: &str) -> Result<Option<String>, ArtError> {
            Ok(self.isrc.clone())
        }
    }

    fn fake_server(link: Option<&str>, isrc: Option<&str>) -> std::sync::Arc<FakeServer> {
        std::sync::Arc::new(FakeServer { link: link.map(Into::into), isrc: isrc.map(Into::into), calls: Default::default() })
    }

    fn hits(counter: &std::sync::Arc<std::sync::atomic::AtomicUsize>) -> usize {
        counter.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[test]
    fn server_cover_is_checked_once_then_used() {
        let server = fake_server(Some(LINK), None);
        let shared = PublicAddress::default();
        shared.set_custom(Some(address("https://m.example.org")));
        let cover = ServerCover::new(server.clone(), shared.clone()).unwrap();
        // The fake can't do https, but the check is the same GET either way. A page that isn't an image, like a login
        // page or proxy error, doesn't count.
        let (page, _) = fake(vec![("/share/img/", 200, "{}".into())]);
        assert!(!cover.loads(&format!("{page}/share/img/x")));
        let (ok_base, served) = fake_image(200);
        assert!(cover.loads(&format!("{ok_base}/share/img/x")));
        assert_eq!(hits(&served), 1);
        let (bad_base, _) = fake_image(404);
        assert!(!cover.loads(&format!("{bad_base}/share/img/x")));
        // Once checked, every album's link is used without loading it again.
        let key = "https://m.example.org/".to_owned();
        cover.checked.lock().unwrap().insert(key.clone(), (true, Instant::now()));
        let t = TrackInfo { album_id: Some("al".into()), ..song("A", "B", "C") };
        assert_eq!(cover.find(&t).unwrap().as_deref(), Some("https://m.example.org/share/img/eyJhbGciOiJIUzI1NiJ9.eyJpZCI6ImFsLTEifQ.c2ln?size=1200"));
        // A failed address isn't asked again for a while.
        cover.checked.lock().unwrap().insert(key, (false, Instant::now()));
        assert_eq!(cover.find(&t).unwrap(), None);
        assert_eq!(server.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        // No address means nothing is asked, and a new address is a new generation.
        let before = shared.generation();
        shared.set_custom(None);
        assert_eq!(cover.find(&t).unwrap(), None);
        assert_eq!(server.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_ne!(shared.generation(), before);
        shared.set_custom(None);
        assert_eq!(shared.generation(), before + 1, "setting the same address changes nothing");
    }

    #[test]
    fn connected_address_qualifies_only_when_it_looks_public() {
        let may = |text: &str| server_address_may_be_public(&Url::parse(text).unwrap());
        assert!(may("https://music.example.org"));
        assert!(may("https://music.example.org:8443/navidrome"));
        assert!(may("https://8.8.8.8"));
        assert!(may("https://[2606:4700::1111]"));
        for local in [
            "http://music.example.org",
            "https://localhost:4533",
            "https://127.0.0.1",
            "https://10.0.0.2:4533",
            "https://172.16.4.1",
            "https://192.168.1.20",
            "https://169.254.3.3",
            "https://100.64.0.1",
            "https://100.101.102.103",
            "https://100.127.255.254",
            "https://[::1]",
            "https://[fd7a:115c:a1e0::1]",
            "https://[fe80::1]",
            "https://[::ffff:192.168.1.2]",
            "https://nas.local",
            "https://NAS.LOCAL.",
            "https://music.localhost",
        ] {
            assert!(!may(local), "{local}");
        }
        // Tailscale Funnel can make a .ts.net name public, so these go to DNS like any other name.
        assert!(may("https://nas.tail1234.ts.net") && may("https://nas.lan") && may("https://nas.home.arpa"));
        // Just outside CGNAT.
        assert!(may("https://100.63.0.1") && may("https://100.128.0.1"));
        // IP literals need no DNS.
        assert_eq!(public_server_address("https://8.8.8.8:4533/nd").unwrap().as_str(), "https://8.8.8.8:4533/nd");
        assert_eq!(public_server_address("https://user:pw@8.8.8.8/?x=1").unwrap().as_str(), "https://8.8.8.8/");
        assert_eq!(public_server_address("https://100.64.1.1"), None);
        assert_eq!(public_server_address("http://8.8.8.8"), None);
        assert_eq!(public_server_address("not an address"), None);
        // A name counts once one of its DNS answers is public. Split DNS answering only private ones doesn't.
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        assert!(!any_public([ip("192.168.1.2"), ip("fd00::2")].into_iter()));
        assert!(any_public([ip("192.168.1.2"), ip("203.0.114.9")].into_iter()));
        assert!(!any_public(std::iter::empty()));
    }

    #[test]
    fn covers_from_each_mode_and_the_filled_address_wins() {
        let custom = || Some(address("https://covers.example.org"));
        let connected = || Some(address("https://8.8.8.8"));
        assert_eq!(cover_address(CoverSource::Automatic, custom(), connected()), custom());
        assert_eq!(cover_address(CoverSource::Automatic, None, connected()), connected());
        assert_eq!(cover_address(CoverSource::Server, custom(), connected()), custom());
        assert_eq!(cover_address(CoverSource::Search, custom(), connected()), None);

        let shared = PublicAddress::default();
        // Automatic uses a public-looking server, a filled-in field wins, and clearing it goes back.
        shared.set_server(Some("https://8.8.8.8:4533"));
        assert_eq!(shared.get().unwrap().as_str(), "https://8.8.8.8:4533/");
        shared.set_custom(custom());
        assert_eq!(shared.get(), custom());
        shared.set_custom(None);
        assert_eq!(shared.get().unwrap().as_str(), "https://8.8.8.8:4533/");
        // Automatic with an obviously local server uses the external sources.
        shared.set_server(Some("https://192.168.1.20:4533"));
        assert_eq!(shared.get(), None);
        assert!(!shared.trusted());
        // My server uses the server's link even when its address looks local, but never over http.
        let before = shared.generation();
        shared.set_source(CoverSource::Server);
        assert_ne!(shared.generation(), before);
        assert_eq!(shared.get().unwrap().as_str(), "https://192.168.1.20:4533/");
        assert!(shared.trusted());
        shared.set_server(Some("http://192.168.1.20:4533"));
        assert_eq!(shared.get(), None);
        shared.set_custom(custom());
        assert_eq!(shared.get(), custom());
        // External sources never use a server link, even with an address filled in.
        shared.set_source(CoverSource::Search);
        assert_eq!(shared.get(), None);
        shared.set_server(Some("https://8.8.8.8"));
        shared.set_custom(None);
        assert_eq!(shared.get(), None);
    }

    #[test]
    fn my_server_skips_the_load_check_and_online_search_skips_the_server() {
        let server = fake_server(Some(LINK), None);
        let shared = PublicAddress::default();
        shared.set_server(Some("https://10.0.0.2:4533"));
        let cover = ServerCover::new(server.clone(), shared.clone()).unwrap();
        let t = TrackInfo { album_id: Some("al".into()), ..song("A", "B", "C") };
        // Automatic doesn't even ask a local address for a link.
        assert_eq!(cover.find(&t).unwrap(), None);
        assert_eq!(server.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        // My server uses it as is, even after a failed test load.
        shared.set_source(CoverSource::Server);
        cover.checked.lock().unwrap().insert("https://10.0.0.2:4533/".into(), (false, Instant::now()));
        assert_eq!(cover.find(&t).unwrap().as_deref(), Some("https://10.0.0.2:4533/share/img/eyJhbGciOiJIUzI1NiJ9.eyJpZCI6ImFsLTEifQ.c2ln?size=1200"));
        // External sources don't ask the server.
        shared.set_source(CoverSource::Search);
        assert_eq!(cover.find(&t).unwrap(), None);
        assert_eq!(server.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// Answers every request with an image, or with `status`.
    fn fake_image(status: u16) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) <= 2 { break }
                }
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Type: image/jpeg\r\nContent-Length: 4\r\nConnection: close\r\n\r\nJPEG");
            }
        });
        (base, hits)
    }

    /// Every outside source on fakes, with counters for art, iTunes hits, Deezer hits and MusicBrainz hits.
    #[allow(clippy::type_complexity)]
    fn chain(deezer: Vec<(&'static str, u16, String)>, itunes: Vec<(&'static str, u16, String)>) -> (PublicArt, [std::sync::Arc<std::sync::atomic::AtomicUsize>; 3]) {
        let (itunes_base, it) = fake(itunes);
        let (deezer_base, dz) = fake(deezer);
        let (mb, mbh) = fake(vec![("/ws/2/release-group", 200, json!({"release-groups": []}).to_string())]);
        (PublicArt::with_bases(&mb, &mb, &itunes_base, &deezer_base).unwrap(), [it, dz, mbh])
    }

    fn deezer_album(title: &str, art: &str) -> String {
        json!({"data": [{"title": title, "artist": {"name": "Tame Impala"}, "cover_xl": art}]}).to_string()
    }

    fn isrc_body(album: &str) -> String {
        isrc_track("Tame Impala", album).to_string()
    }

    #[test]
    fn chain_order_server_then_isrc_then_albums_then_songs() {
        let no_results = || vec![("/search", 200, json!({"results": []}).to_string())];
        let dracula = || TrackInfo { id: "s1".into(), album_id: Some("al-1".into()), isrc: Some("USQX92600464".into()), ..song("Tame Impala • JENNIE", "Dracula (Remix)", "Dracula - JENNIE Remix") };

        // 1. The server's own cover, once its address checked out. Nothing else is asked.
        let (art, [it, dz, mb]) = chain(vec![], no_results());
        let shared = PublicAddress::default();
        shared.set_custom(Some(address("https://m.example.org")));
        let mut art = art.with_server(fake_server(Some(LINK), None), shared).unwrap();
        art.cover.as_mut().unwrap().checked.get_mut().unwrap().insert("https://m.example.org/".into(), (true, Instant::now()));
        let found = art.find_art(&dracula()).unwrap().unwrap();
        assert!(found.album_wide && found.url.starts_with("https://m.example.org/share/img/"));
        assert_eq!((hits(&it), hits(&dz), hits(&mb)), (0, 0, 0));

        // 2. An ISRC hit on our own album is used at once, for this song.
        let (art, [it, dz, mb]) = chain(vec![("/track/isrc:USQX92600464", 200, isrc_body("Dracula (Remix)"))], no_results());
        assert_eq!(art.find_art(&dracula()).unwrap(), Some(Art { url: "https://cdn.example/isrc.jpg".into(), album_wide: false }));
        assert_eq!((hits(&it), hits(&dz), hits(&mb)), (0, 1, 0));

        // 3. An ISRC hit on another album waits for the album searches, which win when they match.
        let (art, _) = chain(vec![("/track/isrc:", 200, isrc_body("The Highlights")), ("/search/album", 200, deezer_album("Dracula (Remix)", "https://cdn.example/album.jpg"))], no_results());
        assert_eq!(art.find_art(&dracula()).unwrap(), Some(Art { url: "https://cdn.example/album.jpg".into(), album_wide: true }));

        // 4. When they miss, the ISRC hit comes before the song search.
        let (art, [_, dz, _]) = chain(vec![("/track/isrc:", 200, isrc_body("The Highlights")), ("/search/album", 200, json!({"data": []}).to_string())], no_results());
        assert_eq!(art.find_art(&dracula()).unwrap(), Some(Art { url: "https://cdn.example/isrc.jpg".into(), album_wide: false }));
        assert_eq!(hits(&dz), 2, "ISRC and album search, no song search");

        // 5. With no ISRC on the track, the server is asked for it.
        let (art, _) = chain(vec![("/track/isrc:USQX92600464", 200, isrc_body("Dracula (Remix)"))], no_results());
        let art = art.with_server(fake_server(None, Some("USQX92600464")), PublicAddress::default()).unwrap();
        let bare = TrackInfo { isrc: None, ..dracula() };
        assert_eq!(art.find_art(&bare).unwrap().map(|a| a.url).as_deref(), Some("https://cdn.example/isrc.jpg"));

        // 6. A wrong artist on the ISRC and nothing else falls through to the song search, then nothing.
        let (art, [_, dz, _]) = chain(vec![("/track/isrc:", 200, isrc_track("Someone Else", "x").to_string()), ("/search/", 200, json!({"data": []}).to_string())], no_results());
        assert_eq!(art.find_art(&dracula()).unwrap(), None);
        assert_eq!(hits(&dz), 3, "ISRC, album search, song search");
    }

    /// Checks the Automatic default against a real server, read-only. Prints no addresses. Run by hand with
    /// `NAVIA_SERVER_COVER_PROBE='<server address>|<getAlbumInfo2 largeImageUrl>' cargo test -p navia-integrations
    /// real_server_cover -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_server_cover() {
        let probe = std::env::var("NAVIA_SERVER_COVER_PROBE").expect("NAVIA_SERVER_COVER_PROBE");
        let (server, link) = probe.split_once('|').unwrap();
        let qualifies = public_server_address(server).is_some();
        println!("connected address qualifies: {qualifies}");
        let shared = PublicAddress::default();
        shared.set_server(Some(server));
        let cover = ServerCover::new(fake_server(Some(link), None), shared).unwrap();
        let found = cover.find(&TrackInfo { album_id: Some("probe".into()), ..song("A", "B", "C") }).unwrap();
        println!("cover link chosen and loaded: {} ({} characters)", found.is_some(), found.as_deref().map_or(0, str::len));
        assert!(qualifies && found.is_some());
    }

    /// Real lookups against the public services, read-only. Run by hand with
    /// `NAVIA_ART_PROBE='Artist|Album[|Title[|ISRC]];...' cargo test -p navia-integrations real_albums -- --ignored
    /// --nocapture`.
    #[test]
    #[ignore]
    fn real_albums() {
        let list = std::env::var("NAVIA_ART_PROBE").unwrap_or_default();
        let itunes = ItunesArt::new().unwrap();
        let deezer = DeezerArt::new().unwrap();
        for entry in list.split(';').filter(|p| p.contains('|')) {
            let mut fields = entry.split('|');
            let (artist, album, title) = (fields.next().unwrap(), fields.next().unwrap(), fields.next().unwrap_or(""));
            let isrc = fields.next().filter(|i| !i.is_empty());
            let t = song(artist, album, title);
            if let Some(isrc) = isrc {
                println!("{artist} | {album} | {title} | {isrc}\n  deezer isrc: {:?}", deezer.find_isrc(isrc, &t));
            }
            println!("{artist} | {album} | {title}\n  itunes: {:?}\n  deezer: {:?}", itunes.find(&t), deezer.find(&t));
            if !title.is_empty() {
                println!("  deezer song: {:?}\n  itunes song: {:?}", deezer.find_track(&t), itunes.find_track(&t));
            }
        }
    }

    #[test]
    fn mbids_are_validated() {
        assert!(is_mbid(ID_A));
        assert!(!is_mbid("../../etc/passwd"));
        assert!(!is_mbid(""));
    }
}
