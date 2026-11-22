//! Cover art with a disk cache. Each image also yields a tint and a pre-blurred glow, since neither GPUI
//! nor SwiftUI blurs cheaply at draw time.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use image::{ImageBuffer, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Semaphore};

use crate::client::Client;
use crate::error::{Error, Result};

mod animated;
pub use animated::AnimatedArt;
use animated::Sniff;

/// The artwork cache's size limit. Images average about 70 KB, so this holds around 30,000,
/// enough for every cover and photo of a large library at the sizes the apps ask for, so art
/// doesn't reload while browsing.
pub const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Concurrent server fetches: a browser's per-host limit, still gentle on shared servers.
const MAX_CONCURRENT_FETCHES: usize = 6;
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// how long an image the server marked `no-store` is reused before it's asked for again. Navidrome
/// sends its no-cover placeholder that way, so a cover added later shows up within this.
pub const NO_STORE_TTL: std::time::Duration = std::time::Duration::from_secs(10 * 60);
/// remembers the images seen marked `no-store` (length and md5 per line), so copies cached before
/// they were recognised are dropped when next read.
const NO_STORE_LIST: &str = "no-store.txt";
/// file name start of the shared copies of `no-store` images.
const NO_STORE_PREFIX: &str = "no-store-";
/// an original can be several megabytes, so it gets longer than a sized image.
const ORIGINAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

#[derive(Clone, Debug)]
pub struct ArtworkConfig {
    pub dir: PathBuf,
    /// Cache size limit. Oldest files go first when it's exceeded.
    pub max_bytes: u64,
}

/// A cached cover image and what was derived from it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoverArt {
    /// always beside the sidecar, never read from it.
    #[serde(skip)]
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Dominant color of the art, as RGB.
    pub tint: [u8; 3],
    /// the server said not to store it (Navidrome's no-cover placeholder): show it, but ask again
    /// after [`NO_STORE_TTL`], since a real cover may have been added by then.
    #[serde(skip)]
    pub placeholder: bool,
}

/// A pre-blurred glow image, stored as a PNG.
#[derive(Clone, Debug)]
pub struct GlowImage {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone)]
pub struct ArtworkCache {
    inner: Arc<Inner>,
}

struct Inner {
    client: Client,
    config: ArtworkConfig,
    limit: Semaphore,
    /// Per-key locks so two requests for one image share a single download.
    keys: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Bytes written since the folder was last measured. The first write measures it.
    since_trim: AtomicU64,
    /// images the server said not to store, by file base, until they expire.
    unstored: std::sync::Mutex<HashMap<PathBuf, (CoverArt, std::time::Instant)>>,
    /// length and md5 of each image seen marked `no-store`.
    no_store: std::sync::Mutex<HashSet<(u64, String)>>,
    /// covers whose failure was already logged, so a broken one is reported once per run.
    failed: std::sync::Mutex<HashSet<String>>,
    /// one animated cover decodes at a time, so only one cover's full-size frames are in memory.
    decode: Semaphore,
}

impl ArtworkCache {
    pub fn new(client: Client, config: ArtworkConfig) -> Result<Self> {
        std::fs::create_dir_all(&config.dir)?;
        let no_store = read_no_store(&config.dir.join(NO_STORE_LIST));
        Ok(Self {
            inner: Arc::new(Inner {
                client,
                config,
                limit: Semaphore::new(MAX_CONCURRENT_FETCHES),
                keys: Mutex::new(HashMap::new()),
                since_trim: AtomicU64::new(u64::MAX / 2),
                unstored: Default::default(),
                no_store: std::sync::Mutex::new(no_store),
                failed: Default::default(),
                decode: Semaphore::new(1),
            }),
        })
    }

    fn key_lock(&self, key: &str) -> impl std::future::Future<Output = Arc<Mutex<()>>> + '_ {
        let key = key.to_string();
        async move {
            let mut keys = self.inner.keys.lock().await;
            keys.entry(key).or_default().clone()
        }
    }

    fn file_base(&self, cover_art_id: &str, size: Option<u32>) -> PathBuf {
        let tag = size.map(|s| s.to_string()).unwrap_or_else(|| "orig".to_string());
        let hash = format!("{:x}", md5::compute(format!("{cover_art_id}\0{tag}")));
        self.inner.config.dir.join(hash)
    }

    /// The cover at `size` pixels (the original if `None`), from cache or the server.
    pub async fn cover(&self, cover_art_id: &str, size: Option<u32>) -> Result<CoverArt> {
        let base = self.file_base(cover_art_id, size);
        let lock = self.key_lock(&base.to_string_lossy()).await;
        let _guard = lock.lock().await;
        if let Some(found) = self.cached(&base) {
            touch(&found.path);
            return Ok(found);
        }
        let fetched = self.fetch(cover_art_id, size, base).await;
        if let Err(e) = &fetched
            && self.inner.failed.lock().unwrap().insert(cover_art_id.to_string())
        {
            tracing::warn!("cover {cover_art_id} at {size:?} failed: {e}");
        }
        fetched
    }

    fn cached(&self, base: &Path) -> Option<CoverArt> {
        {
            let mut unstored = self.inner.unstored.lock().unwrap();
            match unstored.get(base) {
                Some((cover, at)) if at.elapsed() < NO_STORE_TTL && cover.path.exists() => return Some(cover.clone()),
                Some(_) => {
                    unstored.remove(base);
                    return None;
                }
                None => {}
            }
        }
        let found = read_sidecar(base)?;
        if self.is_no_store_copy(&found.path) {
            let _ = std::fs::remove_file(sidecar_path(base));
            let _ = std::fs::remove_file(&found.path);
            return None;
        }
        Some(found)
    }

    /// whether a cached file is one of the images the server marked `no-store`. only a file of a
    /// known length is hashed.
    fn is_no_store_copy(&self, path: &Path) -> bool {
        let Ok(len) = std::fs::metadata(path).map(|m| m.len()) else { return false };
        let known = self.inner.no_store.lock().unwrap();
        if !known.iter().any(|(l, _)| *l == len) {
            return false;
        }
        std::fs::read(path).is_ok_and(|bytes| known.contains(&(len, format!("{:x}", md5::compute(&bytes)))))
    }

    async fn fetch(&self, cover_art_id: &str, size: Option<u32>, base: PathBuf) -> Result<CoverArt> {
        let (bytes, no_store) = {
            let _permit = self.inner.limit.acquire().await.map_err(|e| Error::Audio(format!("artwork limiter: {e}")))?;
            let url = self.inner.client.cover_art_url(cover_art_id, size)?;
            // Bounded, so a stalled connection can't hold a slot and starve the images behind it.
            let response = self.inner.client.fetch(&url).await.timeout(FETCH_TIMEOUT).send().await?;
            let status = response.status();
            if !status.is_success() {
                return Err(Error::HttpStatus { status: status.as_u16(), endpoint: "getCoverArt".to_string() });
            }
            let no_store = response.headers().get_all(reqwest::header::CACHE_CONTROL).iter().filter_map(|v| v.to_str().ok()).any(says_no_store);
            (response.bytes().await?, no_store)
        };
        if no_store {
            return self.keep_unstored(base, bytes).await;
        }
        let path = base.with_extension("img");
        let marker = self.animated_marker(cover_art_id);
        let cover = tokio::task::spawn_blocking(move || -> Result<(CoverArt, u64)> {
            let image = decode(&bytes)?;
            // servers that keep animation at a size (Navidrome with ffmpeg's animated WebP encoder)
            // give it away for free. a still answer proves nothing: the original may be a GIF.
            if animated::sniff(&bytes) == Sniff::Animated {
                let _ = atomic_write(&marker, b"1");
            }
            atomic_write(&path, &bytes)?;
            let written = bytes.len() as u64;
            let cover = CoverArt { path, width: image.width(), height: image.height(), tint: dominant_tint(&image), placeholder: false };
            atomic_write(&sidecar_path(&base), serde_json::to_string(&cover).map_err(|e| Error::Sync(e.to_string()))?.as_bytes())?;
            Ok((cover, written))
        })
        .await
        .map_err(|e| Error::DatabaseTask(e.to_string()))??;
        self.spawn_trim(cover.1);
        Ok(cover.0)
    }

    /// an image the server said not to store (Navidrome's placeholder for a missing cover). apps
    /// need a file, so one copy per distinct image is kept on disk, named by its content, with no
    /// sidecar; each request remembers it only for [`NO_STORE_TTL`].
    async fn keep_unstored(&self, base: PathBuf, bytes: impl AsRef<[u8]> + Send + 'static) -> Result<CoverArt> {
        let len = bytes.as_ref().len() as u64;
        let digest = format!("{:x}", md5::compute(bytes.as_ref()));
        let path = self.inner.config.dir.join(format!("{NO_STORE_PREFIX}{digest}")).with_extension("img");
        let cover = tokio::task::spawn_blocking(move || -> Result<CoverArt> {
            let image = decode(bytes.as_ref())?;
            if !path.exists() {
                atomic_write(&path, bytes.as_ref())?;
            }
            Ok(CoverArt { path, width: image.width(), height: image.height(), tint: dominant_tint(&image), placeholder: true })
        })
        .await
        .map_err(|e| Error::DatabaseTask(e.to_string()))??;
        let learned = self.inner.no_store.lock().unwrap().insert((len, digest));
        if learned {
            let list = self.inner.no_store.lock().unwrap().clone();
            let _ = write_no_store(&self.inner.config.dir.join(NO_STORE_LIST), &list);
        }
        self.inner.unstored.lock().unwrap().insert(base, (cover.clone(), std::time::Instant::now()));
        Ok(cover)
    }

    /// A glow image for this cover at the given size, built once and cached.
    pub async fn glow(&self, cover_art_id: &str, width: u32, height: u32) -> Result<GlowImage> {
        let source = self.cover(cover_art_id, Some(300)).await?;
        let glow_name = format!("glow.{width}x{height}.png");
        // a placeholder's glow belongs to the placeholder, so it isn't kept for this cover.
        let unstored = source.path.file_name().is_some_and(|n| n.to_string_lossy().starts_with(NO_STORE_PREFIX));
        let path = if unstored { source.path.with_extension(glow_name) } else { self.file_base(cover_art_id, Some(0)).with_extension(glow_name) };
        let lock = self.key_lock(&path.to_string_lossy()).await;
        let _guard = lock.lock().await;
        if path.exists() {
            touch(&path);
        } else {
            let (from, to) = (source.path.clone(), path.clone());
            tokio::task::spawn_blocking(move || write_glow(&from, &to, width, height)).await.map_err(|e| Error::DatabaseTask(e.to_string()))??;
            self.spawn_trim(std::fs::metadata(&path).map_or(0, |m| m.len()));
        }
        Ok(GlowImage { path, width, height })
    }

    fn animated_marker(&self, cover_art_id: &str) -> PathBuf {
        self.file_base(cover_art_id, None).with_extension("animated")
    }

    /// whether this cover plays as an animation, from the cache only: `None` until
    /// `animated_artwork` (or a sized fetch that came back animated) has found out.
    pub fn is_animated(&self, cover_art_id: &str) -> Option<bool> {
        match std::fs::read(self.animated_marker(cover_art_id)).ok()?.first()? {
            b'1' => Some(true),
            b'0' => Some(false),
            _ => None,
        }
    }

    /// the cover's frames for display at `size` pixels (longest side), or `None` for a still
    /// cover. decoded once and kept on disk; later calls only read the frame table. the first call
    /// for a cover fetches the sized image (as `cover` does) and, if that is still, starts the
    /// original and drops the connection as soon as its first bytes show it's still too.
    pub async fn animated_artwork(&self, cover_art_id: &str, size: u32) -> Result<Option<AnimatedArt>> {
        let path = self.file_base(cover_art_id, Some(size)).with_extension("anim");
        let lock = self.key_lock(&path.to_string_lossy()).await;
        let _guard = lock.lock().await;
        let cached = {
            let path = path.clone();
            tokio::task::spawn_blocking(move || AnimatedArt::open(&path)).await.map_err(|e| Error::DatabaseTask(e.to_string()))?
        };
        match cached {
            Ok(Some(art)) => {
                touch(&art.path);
                return Ok(Some(art));
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("animated cover cache unreadable, decoding again: {e}");
                let _ = std::fs::remove_file(&path);
            }
        }
        let marker = self.animated_marker(cover_art_id);
        if self.is_animated(cover_art_id) == Some(false) {
            return Ok(None);
        }
        let sized = self.cover(cover_art_id, Some(size)).await?;
        let sized_bytes = std::fs::read(&sized.path)?;
        let source = if animated::sniff(&sized_bytes) == Sniff::Animated {
            Some(sized_bytes)
        } else {
            drop(sized_bytes);
            self.animated_original(cover_art_id).await?
        };
        let Some(source) = source else {
            atomic_write(&marker, b"0")?;
            return Ok(None);
        };
        let decoded = {
            let _permit = self.inner.decode.acquire().await.map_err(|e| Error::Audio(format!("artwork decoder: {e}")))?;
            let dest = path.clone();
            tokio::task::spawn_blocking(move || animated::decode_to_file(&source, size, &dest)).await.map_err(|e| Error::DatabaseTask(e.to_string()))?
        };
        match decoded {
            Ok(Some(art)) => {
                atomic_write(&marker, b"1")?;
                self.spawn_trim(std::fs::metadata(&art.path).map_or(0, |m| m.len()));
                Ok(Some(art))
            }
            Ok(None) => {
                atomic_write(&marker, b"0")?;
                Ok(None)
            }
            Err(e) => {
                // too large or undecodable: show it still rather than retry on every view.
                tracing::warn!("animated cover {cover_art_id} not playable: {e}");
                atomic_write(&marker, b"0")?;
                Ok(None)
            }
        }
    }

    /// the original's bytes if it is animated. kept on disk, so other sizes decode without the
    /// server; a still original is dropped after its first bytes.
    async fn animated_original(&self, cover_art_id: &str) -> Result<Option<Vec<u8>>> {
        let kept = self.file_base(cover_art_id, None).with_extension("animsrc");
        if let Ok(bytes) = std::fs::read(&kept) {
            touch(&kept);
            return Ok(Some(bytes));
        }
        let _permit = self.inner.limit.acquire().await.map_err(|e| Error::Audio(format!("artwork limiter: {e}")))?;
        let url = self.inner.client.cover_art_url(cover_art_id, None)?;
        let mut response = self.inner.client.fetch(&url).await.timeout(ORIGINAL_TIMEOUT).send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::HttpStatus { status: status.as_u16(), endpoint: "getCoverArt".to_string() });
        }
        let mut bytes = Vec::new();
        let mut animated = false;
        while let Some(chunk) = response.chunk().await? {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > animated::MAX_SOURCE_BYTES {
                tracing::warn!("animated cover {cover_art_id} is over {} bytes, shown still", animated::MAX_SOURCE_BYTES);
                return Ok(None);
            }
            if !animated {
                match animated::sniff(&bytes) {
                    Sniff::Animated => animated = true,
                    // dropping the response closes the connection, so the rest isn't sent.
                    Sniff::Still => return Ok(None),
                    Sniff::NeedMore => {}
                }
            }
        }
        if !animated {
            return Ok(None);
        }
        atomic_write(&kept, &bytes)?;
        self.spawn_trim(bytes.len() as u64);
        Ok(Some(bytes))
    }

    /// Remove the oldest cached files until the size limit holds.
    pub fn trim(&self) -> Result<u64> {
        trim_dir(&self.inner.config.dir, self.inner.config.max_bytes)
    }

    /// Bytes the cached images take on disk.
    pub fn usage(&self) -> u64 {
        let Ok(entries) = std::fs::read_dir(&self.inner.config.dir) else { return 0 };
        entries.flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum()
    }

    /// Deletes every cached image. Images already on screen stay in the apps' memory.
    pub fn clear(&self) {
        self.inner.unstored.lock().unwrap().clear();
        let Ok(entries) = std::fs::read_dir(&self.inner.config.dir) else { return };
        for entry in entries.flatten() {
            if entry.metadata().is_ok_and(|m| m.is_file()) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// Trims after about a twentieth of the limit has been written, so the folder isn't listed for every image.
    fn spawn_trim(&self, written: u64) {
        let step = (self.inner.config.max_bytes / 20).max(1);
        if self.inner.since_trim.fetch_add(written, Ordering::AcqRel).saturating_add(written) < step {
            return;
        }
        self.inner.since_trim.store(0, Ordering::Release);
        let cache = self.clone();
        tokio::spawn(async move {
            let _ = tokio::task::spawn_blocking(move || cache.trim()).await;
        });
    }
}

fn sidecar_path(base: &Path) -> PathBuf {
    base.with_extension("json")
}

/// the cached cover for a file base. the image's path comes from the base, never from the sidecar.
fn read_sidecar(base: &Path) -> Option<CoverArt> {
    let text = std::fs::read_to_string(sidecar_path(base)).ok()?;
    let mut cover: CoverArt = serde_json::from_str(&text).ok()?;
    cover.path = base.with_extension("img");
    cover.path.exists().then_some(cover)
}

fn decode(bytes: &[u8]) -> Result<RgbaImage> {
    Ok(image::load_from_memory(bytes).map_err(|e| Error::Decode { endpoint: "getCoverArt".to_string(), detail: e.to_string() })?.into_rgba8())
}

fn says_no_store(cache_control: &str) -> bool {
    cache_control.split(',').any(|d| d.trim().eq_ignore_ascii_case("no-store"))
}

fn read_no_store(path: &Path) -> HashSet<(u64, String)> {
    let Ok(text) = std::fs::read_to_string(path) else { return HashSet::new() };
    text.lines().filter_map(|l| l.split_once(' ')).filter_map(|(len, md5)| Some((len.parse().ok()?, md5.to_string()))).collect()
}

fn write_no_store(path: &Path, known: &HashSet<(u64, String)>) -> Result<()> {
    let text: String = known.iter().map(|(len, md5)| format!("{len} {md5}\n")).collect();
    atomic_write(path, text.as_bytes())
}

fn touch(path: &Path) {
    let now = std::time::SystemTime::now();
    if let Ok(f) = std::fs::File::options().write(true).open(path) {
        let _ = f.set_modified(now);
    }
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn trim_dir(dir: &Path, max_bytes: u64) -> Result<u64> {
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    let mut total = 0u64;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if !meta.is_file() {
            continue;
        }
        total += meta.len();
        files.push((meta.modified().unwrap_or(std::time::UNIX_EPOCH), meta.len(), entry.path()));
    }
    files.sort();
    let mut removed = 0;
    for (_, len, path) in files {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
            removed += len;
            // A sidecar without its image reads as a miss, so it's harmless, but tidy it up.
            if path.extension().is_some_and(|e| e == "img") {
                let _ = std::fs::remove_file(path.with_extension("json"));
            }
        }
    }
    Ok(removed)
}

/// The most prominent color: strongest hue bin weighted by saturation and brightness.
/// Falls back to the average for grayscale art.
pub fn dominant_tint(image: &RgbaImage) -> [u8; 3] {
    let small = resize_rgba(image, 32, 32, FilterType::Bilinear);
    const BINS: usize = 24;
    let mut weight = [0.0f32; BINS];
    let mut color = [[0.0f32; 3]; BINS];
    let mut total_rgb = [0.0f32; 3];
    let mut count = 0.0f32;
    for px in small.pixels() {
        if px.0[3] < 128 {
            continue;
        }
        let [r, g, b] = [px.0[0] as f32, px.0[1] as f32, px.0[2] as f32];
        total_rgb[0] += r;
        total_rgb[1] += g;
        total_rgb[2] += b;
        count += 1.0;
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let chroma = max - min;
        if chroma < 16.0 {
            continue;
        }
        let hue = if max == r {
            ((g - b) / chroma).rem_euclid(6.0)
        } else if max == g {
            (b - r) / chroma + 2.0
        } else {
            (r - g) / chroma + 4.0
        } / 6.0;
        let bin = ((hue * BINS as f32) as usize).min(BINS - 1);
        let saturation = chroma / max.max(1.0);
        let w = saturation * (max / 255.0);
        weight[bin] += w;
        color[bin][0] += r * w;
        color[bin][1] += g * w;
        color[bin][2] += b * w;
    }
    let best = weight.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, w)| (i, *w)).unwrap_or((0, 0.0));
    if best.1 > 0.0 {
        let w = weight[best.0];
        [(color[best.0][0] / w) as u8, (color[best.0][1] / w) as u8, (color[best.0][2] / w) as u8]
    } else if count > 0.0 {
        [(total_rgb[0] / count) as u8, (total_rgb[1] / count) as u8, (total_rgb[2] / count) as u8]
    } else {
        [0, 0, 0]
    }
}

fn resize_rgba(src: &RgbaImage, width: u32, height: u32, filter: FilterType) -> RgbaImage {
    let source = Image::from_vec_u8(src.width(), src.height(), src.as_raw().clone(), PixelType::U8x4).expect("image buffer");
    let mut target = Image::new(width, height, PixelType::U8x4);
    Resizer::new().resize(&source, &mut target, &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(filter))).expect("resize");
    RgbaImage::from_raw(width, height, target.into_vec()).expect("buffer size")
}

fn resize_rgba16(src: &ImageBuffer<Rgba<u16>, Vec<u16>>, width: u32, height: u32, filter: FilterType) -> ImageBuffer<Rgba<u16>, Vec<u16>> {
    let bytes: Vec<u8> = src.as_raw().iter().flat_map(|v| v.to_ne_bytes()).collect();
    let source = Image::from_vec_u8(src.width(), src.height(), bytes, PixelType::U16x4).expect("image buffer");
    let mut target = Image::new(width, height, PixelType::U16x4);
    Resizer::new().resize(&source, &mut target, &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(filter))).expect("resize");
    let out = target.into_vec().chunks_exact(2).map(|b| u16::from_ne_bytes([b[0], b[1]])).collect();
    ImageBuffer::from_raw(width, height, out).expect("buffer size")
}

/// Builds the glow from a tiny image, so its cost doesn't depend on the artwork or output size.
/// Blurs at 16 bits because an 8-bit blur bands.
fn write_glow(source: &Path, dest: &Path, width: u32, height: u32) -> Result<()> {
    let bytes = std::fs::read(source)?;
    let art = image::load_from_memory(&bytes).map_err(|e| Error::Decode { endpoint: "glow".to_string(), detail: e.to_string() })?.into_rgba8();
    let mut small = resize_rgba(&art, 64, 64, FilterType::Bilinear);
    for px in small.pixels_mut() {
        let [r, g, b, _] = px.0.map(f32::from);
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        // Saturate and darken so the glow reads as color, not as a blurred picture.
        let boost = |c: f32| ((luma + (c - luma) * 1.5) * 0.7).clamp(0.0, 255.0) as u8;
        px.0 = [boost(r), boost(g), boost(b), 255];
    }
    let small16: ImageBuffer<Rgba<u16>, Vec<u16>> = ImageBuffer::from_fn(64, 64, |x, y| Rgba(small.get_pixel(x, y).0.map(|c| u16::from(c) * 257)));
    let blurred = image::imageops::blur(&small16, 11.0);
    let upscaled = resize_rgba16(&blurred, width, height, FilterType::CatmullRom);
    let mut full = RgbaImage::new(width, height);
    // Triangular dither of about one 8-bit step, so dark gradients do not band.
    let mut rng = 0x9e37_79b9u32;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        (rng >> 16) as f32 / 65535.0
    };
    for (src, dst) in upscaled.pixels().zip(full.pixels_mut()) {
        for ch in 0..3 {
            let noise = next() - next();
            dst.0[ch] = (f32::from(src.0[ch]) / 257.0 + noise).round().clamp(0.0, 255.0) as u8;
        }
        dst.0[3] = 255;
    }
    let mut encoded = Vec::new();
    full.write_to(&mut std::io::Cursor::new(&mut encoded), image::ImageFormat::Png).map_err(|e| Error::Decode { endpoint: "glow".to_string(), detail: e.to_string() })?;
    atomic_write(dest, &encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tint_finds_the_dominant_hue_in_mostly_gray_art() {
        let image = ImageBuffer::from_fn(100, 100, |x, _| if x < 60 { Rgba([200u8, 40, 30, 255]) } else { Rgba([128u8, 128, 128, 255]) });
        let [r, g, b] = dominant_tint(&image);
        assert!(r > 150 && g < 90 && b < 90, "tint {r} {g} {b}");
    }

    #[test]
    fn grayscale_art_falls_back_to_the_average() {
        let image = ImageBuffer::from_pixel(50, 50, Rgba([90u8, 90, 90, 255]));
        assert_eq!(dominant_tint(&image), [90, 90, 90]);
    }

    #[test]
    fn trim_removes_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        for (i, name) in ["a.img", "b.img", "c.img"].iter().enumerate() {
            std::fs::write(dir.path().join(name), vec![0u8; 100]).unwrap();
            let f = std::fs::File::options().write(true).open(dir.path().join(name)).unwrap();
            f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1000 + i as u64)).unwrap();
        }
        let removed = trim_dir(dir.path(), 200).unwrap();
        assert_eq!(removed, 100);
        assert!(!dir.path().join("a.img").exists());
        assert!(dir.path().join("c.img").exists());
    }
}
