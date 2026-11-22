//! Streaming and download quality, either the original file or a server transcode.
//! Original is the default and streams byte for byte. A transcode costs server CPU and can't be bit-exact.
//! Servers may quietly send another format, so what arrives is checked against what was asked for.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::{Client, ReplayGain, Song, StreamOptions, WritePolicy};
use crate::error::{Error, Result};
use crate::library::Library;
use crate::songcache::SongCache;

/// A format the server can transcode to and the core can play.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    Mp3,
    Opus,
    Aac,
}

impl Codec {
    pub const ALL: [Codec; 3] = [Codec::Mp3, Codec::Opus, Codec::Aac];

    pub fn id(self) -> &'static str {
        match self {
            Codec::Mp3 => "mp3",
            Codec::Opus => "opus",
            Codec::Aac => "aac",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Codec::Mp3 => "MP3",
            Codec::Opus => "Opus",
            Codec::Aac => "AAC",
        }
    }

    pub fn from_id(id: &str) -> Option<Codec> {
        Codec::ALL.into_iter().find(|c| c.id().eq_ignore_ascii_case(id))
    }

    /// File name extension for a downloaded transcode of this format.
    pub fn extension(self) -> &'static str {
        self.id()
    }

    /// Bitrate choices offered for this format, in kbps, lowest first.
    pub fn bitrates(self) -> &'static [u32] {
        match self {
            Codec::Mp3 | Codec::Aac => &[128, 192, 256, 320],
            Codec::Opus => &[96, 128, 160, 192, 256],
        }
    }

    /// The bitrate Navidrome ships as this format's default.
    pub fn default_kbps(self) -> u32 {
        match self {
            Codec::Mp3 => 192,
            Codec::Opus => 128,
            Codec::Aac => 256,
        }
    }

    /// Frames of encoder delay at the start of a piped transcode. ffmpeg can't write gapless data
    /// to a pipe, so this silence plays unless it's trimmed. Opus carries its own pre-skip.
    pub fn encoder_delay(self) -> u64 {
        match self {
            Codec::Mp3 => 1105,
            Codec::Aac => 1024,
            Codec::Opus => 0,
        }
    }

    /// False when this build has no decoder for the format. Apple builds have no native Opus.
    pub fn decodable(self) -> bool {
        match self {
            Codec::Opus => cfg!(feature = "opus"),
            Codec::Mp3 | Codec::Aac => true,
        }
    }

    /// Whether a response's content type is plausibly this format.
    /// Servers fall back silently to their default format or the original when they can't make it.
    pub fn content_type_matches(self, content_type: &str) -> bool {
        let t = content_type.to_ascii_lowercase();
        // A server that sends nothing useful gets the benefit of the doubt.
        if t.is_empty() || t.contains("octet-stream") {
            return true;
        }
        match self {
            Codec::Mp3 => t.contains("mpeg") || t.contains("mp3"),
            Codec::Opus => t.contains("ogg") || t.contains("opus"),
            Codec::Aac => t.contains("aac") || t.contains("mp4"),
        }
    }
}

/// The formats this build offers, in display order.
pub fn offered_codecs() -> Vec<Codec> {
    Codec::ALL.into_iter().filter(|c| c.decodable()).collect()
}

/// What the user picked for streaming, or for downloads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "format")]
pub enum Quality {
    #[default]
    Original,
    Transcode {
        codec: Codec,
        kbps: u32,
    },
}

impl Quality {
    /// A quality from a format id and a kbps limit.
    /// Refuses formats this build can't decode and bitrates not offered for the format, without snapping.
    pub fn new(format: &str, kbps: u32) -> Result<Quality> {
        if format.eq_ignore_ascii_case("original") {
            return Ok(Quality::Original);
        }
        let codec = Codec::from_id(format).filter(|c| c.decodable()).ok_or_else(|| Error::Audio(format!("{format} isn't a format Navia can play here")))?;
        if !codec.bitrates().contains(&kbps) {
            return Err(Error::Audio(format!("{kbps} kbps isn't offered for {}", codec.label())));
        }
        Ok(Quality::Transcode { codec, kbps })
    }

    /// The format id: `original`, `mp3`, `opus` or `aac`.
    pub fn format_id(self) -> &'static str {
        match self {
            Quality::Original => "original",
            Quality::Transcode { codec, .. } => codec.id(),
        }
    }

    pub fn kbps(self) -> u32 {
        match self {
            Quality::Original => 0,
            Quality::Transcode { kbps, .. } => kbps,
        }
    }
}

/// What the library knows about the original file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceInfo {
    pub suffix: Option<String>,
    pub bit_rate: Option<u32>,
    pub size: Option<u64>,
    pub duration_s: Option<u32>,
}

impl SourceInfo {
    pub fn from_song(song: &Song) -> SourceInfo {
        SourceInfo { suffix: song.suffix.clone(), bit_rate: song.bit_rate.filter(|&b| b > 0).map(|b| b as u32), size: song.size.filter(|&s| s > 0).map(|s| s as u64), duration_s: song.duration.filter(|&d| d > 0).map(|d| d as u32) }
    }
}

/// What to do for one song at a chosen quality.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Play or download the original file.
    Original,
    Transcode { codec: Codec, kbps: u32 },
}

/// A file already at or below the chosen limit is left alone, as the server does when the format matches.
/// Re-encoding a lossy file at a similar rate would only lose quality.
pub fn plan(quality: Quality, source: Option<&SourceInfo>) -> Plan {
    let Quality::Transcode { codec, kbps } = quality else {
        return Plan::Original;
    };
    match source.and_then(|s| s.bit_rate) {
        Some(bit_rate) if bit_rate <= kbps => Plan::Original,
        _ => Plan::Transcode { codec, kbps },
    }
}

/// What the audible song is being delivered as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivered {
    /// The original file, unchanged.
    Original,
    /// From disk, downloaded as this format and rate, or `None` for the original file.
    Downloaded { quality: Option<(Codec, u32)> },
    /// From the song cache, as this format and rate, or `None` for the original file.
    Cached { quality: Option<(Codec, u32)> },
    Transcoded { codec: Codec, kbps: u32 },
    /// The chosen quality couldn't be used, so the original file plays instead.
    Fallback { wanted: Codec, kbps: u32, reason: String },
}

impl Delivered {
    /// Short text for the player, like `MP3 192`. Empty for the original file.
    pub fn label(&self) -> String {
        match self {
            Delivered::Transcoded { codec, kbps } | Delivered::Downloaded { quality: Some((codec, kbps)) } | Delivered::Cached { quality: Some((codec, kbps)) } => format!("{} {kbps}", codec.label()),
            _ => String::new(),
        }
    }

    /// The reason this isn't what was asked for, when it isn't.
    pub fn fallback_reason(&self) -> Option<String> {
        match self {
            Delivered::Fallback { wanted, kbps, reason } => Some(format!("Playing the original instead of {} {kbps}: {reason}", wanted.label())),
            _ => None,
        }
    }
}

/// Where a transcode comes from, once the server has been asked.
pub enum Route {
    /// The server says the original file is fine for this limit.
    Original,
    Transcode { url: String },
    /// The server can't make this format for this song.
    Unavailable(String),
}

/// Volume leveling from the server's ReplayGain data.
/// Off is the default and keeps playback bit-exact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Leveling {
    #[default]
    Off,
    Track,
    Album,
}

impl Leveling {
    pub fn id(self) -> &'static str {
        match self {
            Leveling::Off => "off",
            Leveling::Track => "track",
            Leveling::Album => "album",
        }
    }

    pub fn from_id(id: &str) -> Option<Leveling> {
        match id.to_ascii_lowercase().as_str() {
            "off" => Some(Leveling::Off),
            "track" => Some(Leveling::Track),
            "album" => Some(Leveling::Album),
            _ => None,
        }
    }

    /// The linear gain for a song's ReplayGain data, capped by the peak so it can't clip.
    /// A missing value falls back to the other mode's, then `fallbackGain`. Off and no data give exactly 1.0.
    pub fn linear_gain(self, data: &ReplayGain) -> f32 {
        let (db, peak) = match self {
            Leveling::Off => return 1.0,
            Leveling::Track => (data.track_gain.or(data.album_gain), data.track_peak.or(data.album_peak)),
            Leveling::Album => (data.album_gain.or(data.track_gain), data.album_peak.or(data.track_peak)),
        };
        let Some(db) = db.or(data.fallback_gain) else {
            return 1.0;
        };
        let gain = 10f64.powf(db / 20.0);
        let capped = match peak {
            Some(p) if p > 0.0 => gain.min(1.0 / p),
            _ => gain,
        };
        capped as f32
    }
}

/// Stored in the library, so it's per server and per user.
const STREAMING_KEY: &str = "quality.streaming";
const METERED_KEY: &str = "quality.metered";
const DOWNLOAD_KEY: &str = "quality.download";
const LEVELING_KEY: &str = "quality.leveling";
const DOWNLOAD_ON_METERED_KEY: &str = "quality.download_on_metered";

type MeteredWatcher = Box<dyn Fn() + Send + Sync>;

/// The current quality settings and the server negotiation behind them.
pub struct QualityControl {
    library: Library,
    client: Client,
    http: reqwest::blocking::Client,
    streaming: RwLock<Quality>,
    metered_streaming: RwLock<Quality>,
    download: RwLock<Quality>,
    /// Whether the current connection counts as metered. Unknown counts as not metered.
    metered: AtomicBool,
    /// Whether automatic downloads of synced playlists and albums run on a metered connection.
    download_on_metered: AtomicBool,
    /// Called when the metered signal or that setting changes.
    watchers: Mutex<Vec<MeteredWatcher>>,
    leveling: RwLock<Leveling>,
    extension: OnceLock<bool>,
    /// Formats the extension can't make on this server, so `stream` is used for them.
    /// Navidrome 0.64 leaves AAC out of `getTranscodeDecision` even though `stream` makes it.
    extension_refused: Mutex<HashSet<Codec>>,
    /// Songs kept as they stream, originals and transcodes alike.
    cache: Option<Arc<SongCache>>,
}

impl QualityControl {
    /// `cache_dir` holds the song cache, if there is to be one.
    pub fn new(library: Library, client: Client, cache_dir: Option<PathBuf>) -> Result<Arc<Self>> {
        let http = reqwest::blocking::Client::builder()
            .user_agent(crate::client::user_agent())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()?;
        let streaming = load_quality(&library, STREAMING_KEY);
        let metered_streaming = load_quality(&library, METERED_KEY);
        let download = load_quality(&library, DOWNLOAD_KEY);
        let leveling = library.meta(LEVELING_KEY).ok().flatten().and_then(|v| Leveling::from_id(&v)).unwrap_or_default();
        let cache = cache_dir.map(|dir| SongCache::open(dir, Some(library.clone()))).transpose()?;
        let download_on_metered = library.meta(DOWNLOAD_ON_METERED_KEY).ok().flatten().is_some_and(|v| v == "1");
        Ok(Arc::new(Self { library, client, http, streaming: RwLock::new(streaming), metered_streaming: RwLock::new(metered_streaming), download: RwLock::new(download), metered: AtomicBool::new(false), download_on_metered: AtomicBool::new(download_on_metered), watchers: Mutex::new(Vec::new()), leveling: RwLock::new(leveling), extension: OnceLock::new(), extension_refused: Mutex::new(HashSet::new()), cache }))
    }

    pub fn streaming(&self) -> Quality {
        *self.streaming.read().expect("quality")
    }

    pub fn metered_streaming(&self) -> Quality {
        *self.metered_streaming.read().expect("quality")
    }

    /// The quality to stream at now, the metered choice on a metered connection.
    pub fn streaming_now(&self) -> Quality {
        if self.is_metered() { self.metered_streaming() } else { self.streaming() }
    }

    /// Whether the current connection counts as metered.
    pub fn is_metered(&self) -> bool {
        self.metered.load(Ordering::Acquire)
    }

    /// Sets the system's metered signal. The playing track keeps going and the next opens at the new quality.
    pub fn set_metered(&self, metered: bool) {
        if self.metered.swap(metered, Ordering::AcqRel) != metered {
            self.tell_watchers();
        }
    }

    /// Whether synced playlists and albums keep downloading on a metered connection. Off by default.
    /// Downloads the user starts run either way.
    pub fn download_on_metered(&self) -> bool {
        self.download_on_metered.load(Ordering::Acquire)
    }

    pub fn set_download_on_metered(&self, on: bool) -> Result<()> {
        self.library.set_meta(DOWNLOAD_ON_METERED_KEY, Some(if on { "1" } else { "0" }))?;
        if self.download_on_metered.swap(on, Ordering::AcqRel) != on {
            self.tell_watchers();
        }
        Ok(())
    }

    /// Whether automatic downloads wait right now because the connection is metered.
    pub fn automatic_downloads_paused(&self) -> bool {
        self.is_metered() && !self.download_on_metered()
    }

    /// Calls `watcher` whenever the metered signal or "Download on metered connections" changes.
    /// It runs on the caller's thread, so it must only signal.
    pub fn watch_metered(&self, watcher: impl Fn() + Send + Sync + 'static) {
        self.watchers.lock().expect("watchers").push(Box::new(watcher));
    }

    fn tell_watchers(&self) {
        for watcher in self.watchers.lock().expect("watchers").iter() {
            watcher();
        }
    }

    pub fn download(&self) -> Quality {
        *self.download.read().expect("quality")
    }

    pub fn set_streaming(&self, quality: Quality) -> Result<()> {
        *self.streaming.write().expect("quality") = quality;
        self.library.set_meta(STREAMING_KEY, Some(&store_quality(quality)))
    }

    pub fn set_metered_streaming(&self, quality: Quality) -> Result<()> {
        *self.metered_streaming.write().expect("quality") = quality;
        self.library.set_meta(METERED_KEY, Some(&store_quality(quality)))
    }

    pub fn set_download(&self, quality: Quality) -> Result<()> {
        *self.download.write().expect("quality") = quality;
        self.library.set_meta(DOWNLOAD_KEY, Some(&store_quality(quality)))
    }

    pub fn leveling(&self) -> Leveling {
        *self.leveling.read().expect("leveling")
    }

    pub fn set_leveling(&self, mode: Leveling) -> Result<()> {
        *self.leveling.write().expect("leveling") = mode;
        self.library.set_meta(LEVELING_KEY, Some(mode.id()))
    }

    /// A song's ReplayGain data, if the library has any.
    pub fn replay_gain(&self, id: &str) -> Option<ReplayGain> {
        self.library.song(id).ok().flatten().and_then(|s| s.replay_gain)
    }

    /// What the library knows about a song, for planning.
    pub fn source(&self, id: &str) -> Option<SourceInfo> {
        self.library.song(id).ok().flatten().map(|s| SourceInfo::from_song(&s))
    }

    pub fn cache(&self) -> Option<&Arc<SongCache>> {
        self.cache.as_ref()
    }

    /// True when the extension can be used. Read-only clients never use it, since the decision
    /// endpoint can make the server run ffprobe and store the result.
    fn extension_available(&self) -> bool {
        *self.extension.get_or_init(|| {
            if self.client.write_policy() == WritePolicy::ReadOnly {
                return false;
            }
            let Ok(url) = self.client.extensions_url() else { return false };
            let Ok(resp) = self.client.form_post().request_blocking(&self.http, &url).send() else { return false };
            let Ok(value) = resp.json::<Value>() else { return false };
            value.get("subsonic-response").and_then(|r| r.get("openSubsonicExtensions")).and_then(Value::as_array).is_some_and(|list| {
                list.iter().any(|e| e.get("name").and_then(Value::as_str) == Some("transcoding"))
            })
        })
    }

    /// Asks the server for a transcode of `id`, starting `offset_s` whole seconds in.
    pub fn route(&self, id: &str, codec: Codec, kbps: u32, offset_s: u32, token: &Mutex<Option<String>>) -> Result<Route> {
        if !codec.decodable() {
            return Ok(Route::Unavailable(format!("{} can't be played on this device", codec.label())));
        }
        let cached = token.lock().expect("token").clone();
        if let Some(params) = cached {
            return Ok(Route::Transcode { url: self.client.transcode_stream_url(id, &params, offset_s)? });
        }
        if self.extension_available() && !self.extension_refused.lock().expect("refused").contains(&codec) {
            match self.decide(id, codec, kbps) {
                Ok(Decision::Original) => return Ok(Route::Original),
                Ok(Decision::Transcode { params }) => {
                    let url = self.client.transcode_stream_url(id, &params, offset_s)?;
                    *token.lock().expect("token") = Some(params);
                    return Ok(Route::Transcode { url });
                }
                Ok(Decision::Unavailable(reason)) => {
                    tracing::info!("the server can't make {} through the transcoding extension: {reason}", codec.label());
                    self.extension_refused.lock().expect("refused").insert(codec);
                }
                Err(e) if e.is_outage() => return Err(e),
                Err(e) => tracing::warn!("getTranscodeDecision failed, using stream: {e}"),
            }
        }
        let opts = StreamOptions { format: Some(codec.id().to_string()), max_bit_rate: Some(kbps), time_offset: (offset_s > 0).then_some(offset_s) };
        Ok(Route::Transcode { url: self.client.stream_url(id, &opts)? })
    }

    fn decide(&self, id: &str, codec: Codec, kbps: u32) -> Result<Decision> {
        let url = self.client.transcode_decision_url(id)?;
        let resp = self.http.post(url).json(&client_info(codec, kbps)).send().map_err(|e| Error::Network(e.without_url().to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Error::HttpStatus { status: status.as_u16(), endpoint: "getTranscodeDecision".to_string() });
        }
        let value: Value = resp.json().map_err(|e| Error::Network(e.without_url().to_string()))?;
        let body = value.get("subsonic-response").ok_or_else(|| Error::Audio("no response from the decision endpoint".to_string()))?;
        if let Some(error) = body.get("error") {
            return Err(Error::Subsonic { code: error.get("code").and_then(Value::as_i64).unwrap_or(0) as i32, endpoint: "getTranscodeDecision".to_string(), message: error.get("message").and_then(Value::as_str).unwrap_or("").to_string() });
        }
        let d = body.get("transcodeDecision").ok_or_else(|| Error::Audio("no decision in the response".to_string()))?;
        if d.get("canDirectPlay").and_then(Value::as_bool).unwrap_or(false) {
            return Ok(Decision::Original);
        }
        if d.get("canTranscode").and_then(Value::as_bool).unwrap_or(false)
            && let Some(params) = d.get("transcodeParams").and_then(Value::as_str)
        {
            return Ok(Decision::Transcode { params: params.to_string() });
        }
        Ok(Decision::Unavailable(d.get("errorReason").and_then(Value::as_str).unwrap_or("no compatible format").to_string()))
    }

    /// Checks which formats a song can be made into, to mark choices the server can't deliver.
    /// `None` without the extension, since there's no way to tell.
    pub fn detect_formats(&self) -> Option<Vec<(Codec, bool)>> {
        if !self.extension_available() {
            return None;
        }
        let song = self.library.list_songs(crate::library::SongSort::Title, 0, 1, None).ok()?.into_iter().next()?;
        Some(
            offered_codecs()
                .into_iter()
                .map(|codec| {
                    let can = match self.decide(&song.id, codec, 1) {
                        Ok(Decision::Transcode { .. } | Decision::Original) => true,
                        // Navidrome filters AAC out of the extension only, and `stream` still has it.
                        Ok(Decision::Unavailable(_)) => codec == Codec::Aac,
                        Err(_) => true,
                    };
                    (codec, can)
                })
                .collect(),
        )
    }
}

enum Decision {
    Original,
    Transcode { params: String },
    Unavailable(String),
}

/// What the core plays directly and what it would like to be sent instead.
fn client_info(codec: Codec, kbps: u32) -> Value {
    let mut codecs = vec!["flac", "mp3", "aac", "alac", "vorbis", "pcm", "wav"];
    if cfg!(feature = "opus") {
        codecs.push("opus");
    }
    json!({
        "name": "Navia",
        "platform": std::env::consts::OS,
        "maxAudioBitrate": kbps * 1000,
        "maxTranscodingAudioBitrate": kbps * 1000,
        "directPlayProfiles": [{ "protocols": ["http"], "audioCodecs": codecs }],
        "transcodingProfiles": [{ "container": codec.id(), "audioCodec": codec.id(), "protocol": "http", "maxAudioChannels": 2 }],
    })
}

fn load_quality(library: &Library, key: &str) -> Quality {
    library.meta(key).ok().flatten().and_then(|json| serde_json::from_str(&json).ok()).unwrap_or_default()
}

fn store_quality(quality: Quality) -> String {
    serde_json::to_string(&quality).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(bit_rate: Option<u32>) -> SourceInfo {
        SourceInfo { bit_rate, ..Default::default() }
    }

    #[test]
    fn original_is_the_default_and_plans_original() {
        assert_eq!(Quality::default(), Quality::Original);
        assert_eq!(plan(Quality::Original, Some(&src(Some(1000)))), Plan::Original);
    }

    #[test]
    fn at_or_below_the_limit_stays_original() {
        let q = Quality::Transcode { codec: Codec::Mp3, kbps: 192 };
        assert_eq!(plan(q, Some(&src(Some(192)))), Plan::Original);
        assert_eq!(plan(q, Some(&src(Some(128)))), Plan::Original);
        assert_eq!(plan(q, Some(&src(Some(193)))), Plan::Transcode { codec: Codec::Mp3, kbps: 192 });
        assert_eq!(plan(q, Some(&src(None))), Plan::Transcode { codec: Codec::Mp3, kbps: 192 });
        assert_eq!(plan(q, None), Plan::Transcode { codec: Codec::Mp3, kbps: 192 });
    }

    #[test]
    fn quality_validates() {
        assert_eq!(Quality::new("ORIGINAL", 0).unwrap(), Quality::Original);
        assert_eq!(Quality::new("mp3", 320).unwrap(), Quality::Transcode { codec: Codec::Mp3, kbps: 320 });
        assert!(Quality::new("mp3", 300).is_err());
        assert!(Quality::new("wma", 128).is_err());
    }

    #[test]
    fn quality_round_trips() {
        for q in [Quality::Original, Quality::Transcode { codec: Codec::Opus, kbps: 96 }] {
            assert_eq!(serde_json::from_str::<Quality>(&store_quality(q)).unwrap(), q);
        }
    }

    #[test]
    fn content_type_detects_a_substitute() {
        assert!(Codec::Mp3.content_type_matches("audio/mpeg"));
        assert!(!Codec::Mp3.content_type_matches("audio/ogg"));
        assert!(!Codec::Mp3.content_type_matches("audio/flac"));
        assert!(Codec::Opus.content_type_matches("audio/ogg"));
        assert!(Codec::Aac.content_type_matches("audio/aac"));
        assert!(Codec::Aac.content_type_matches(""));
    }

    #[test]
    fn leveling_gain_follows_the_mode_and_caps_by_peak() {
        let data = ReplayGain { track_gain: Some(-6.0), album_gain: Some(-3.0), track_peak: Some(0.5), album_peak: Some(0.9), base_gain: None, fallback_gain: None };
        assert_eq!(Leveling::Off.linear_gain(&data), 1.0);
        assert!((Leveling::Track.linear_gain(&data) - 0.5012).abs() < 1e-3);
        assert!((Leveling::Album.linear_gain(&data) - 0.7079).abs() < 1e-3);
        // A boost the peak can't take is cut back.
        let loud = ReplayGain { track_gain: Some(12.0), track_peak: Some(0.8), ..Default::default() };
        assert!((Leveling::Track.linear_gain(&loud) - 1.25).abs() < 1e-4);
        assert_eq!(Leveling::Track.linear_gain(&ReplayGain::default()), 1.0);
        // The other mode's value stands in when a mode's own is missing.
        let album_only = ReplayGain { album_gain: Some(-6.0), ..Default::default() };
        assert!((Leveling::Track.linear_gain(&album_only) - 0.5012).abs() < 1e-3);
    }

    #[test]
    fn opus_offered_only_with_a_decoder() {
        assert_eq!(offered_codecs().contains(&Codec::Opus), cfg!(feature = "opus"));
    }
}
