//! animated covers (GIF, animated WebP, APNG): told apart from still ones from their first bytes,
//! decoded once at the display size and kept on disk as small per-frame images, so playing one
//! later costs one cheap decode per frame and nothing from the server.

use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use fast_image_resize::FilterType;
use image::codecs::gif::GifDecoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngDecoder;
use image::codecs::qoi::QoiEncoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, Delay, ExtendedColorType, ImageDecoder, ImageEncoder, ImageFormat, RgbaImage};

use crate::error::{Error, Result};

/// longest side kept, whatever size is asked for. covers are never upscaled.
pub const MAX_SIDE: u32 = 1024;
/// the decoded frames of one cover fit in this, so an app can hold them all while it plays.
/// past it the frames get smaller (down to `MIN_SIDE`), then frames are dropped evenly.
pub const MAX_DECODED_BYTES: u64 = 64 * 1024 * 1024;
pub const MIN_SIDE: u32 = 256;
/// frames kept. longer animations keep every second (third, ...) frame with the delays merged,
/// so the timing and the loop stay whole.
pub const MAX_FRAMES: usize = 240;
/// frames read from the source at most; the rest of a longer one is cut off.
pub const MAX_SOURCE_FRAMES: usize = 2000;
/// largest source downloaded or decoded (Navidrome's upload limit is 10 MB by default).
pub const MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;
/// largest source canvas, in pixels, so one full-size frame stays under 64 MB.
const MAX_SOURCE_PIXELS: u64 = 4096 * 4096;
const JPEG_QUALITY: u8 = 90;
/// browsers play GIF delays of 10 ms or less at 100 ms; files are made to look right there.
const FAST_DELAY_MS: u32 = 10;
const FAST_DELAY_AS_MS: u32 = 100;

const MAGIC: &[u8; 4] = b"NVAN";
const VERSION: u8 = 1;
const HEADER_LEN: usize = 20;
const ENTRY_LEN: usize = 9;
const CODEC_JPEG: u8 = 0;
const CODEC_QOI: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sniff {
    Animated,
    Still,
    /// the bytes so far can't tell; read more (at the end of the file it means still).
    NeedMore,
}

/// whether these leading bytes of an image show an animation. GIF needs the bytes up to its
/// second frame; WebP and PNG say so in their first chunks.
pub fn sniff(data: &[u8]) -> Sniff {
    if data.len() < 12 {
        return Sniff::NeedMore;
    }
    if data.starts_with(b"GIF8") {
        let (frames, done) = gif_frames(data, 2);
        return if frames >= 2 {
            Sniff::Animated
        } else if done {
            Sniff::Still
        } else {
            Sniff::NeedMore
        };
    }
    if data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        if data.len() < 21 {
            return Sniff::NeedMore;
        }
        // VP8X flags: bit 1 is the animation flag. simple VP8/VP8L files are always still.
        return if &data[12..16] == b"VP8X" && data[20] & 0x02 != 0 { Sniff::Animated } else { Sniff::Still };
    }
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        // an APNG's acTL chunk comes before the first IDAT.
        let mut pos = 8usize;
        while pos + 8 <= data.len() {
            let len = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
            match &data[pos + 4..pos + 8] {
                b"acTL" => return Sniff::Animated,
                b"IDAT" => return Sniff::Still,
                _ => pos = pos.saturating_add(12).saturating_add(len),
            }
        }
        return Sniff::NeedMore;
    }
    Sniff::Still
}

/// frames in a whole animated file, by walking its blocks (no pixels decoded).
pub fn frame_count(data: &[u8]) -> usize {
    if data.starts_with(b"GIF8") {
        return gif_frames(data, usize::MAX).0;
    }
    if data.starts_with(b"RIFF") && data.len() >= 12 && &data[8..12] == b"WEBP" {
        let (mut pos, mut count) = (12usize, 0);
        while pos + 8 <= data.len() {
            let len = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
            count += usize::from(&data[pos..pos + 4] == b"ANMF");
            pos = pos.saturating_add(8).saturating_add(len + (len & 1));
        }
        return count;
    }
    let mut pos = 8usize;
    while pos + 12 <= data.len() {
        let len = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        if &data[pos + 4..pos + 8] == b"acTL" {
            return u32::from_be_bytes(data[pos + 8..pos + 12].try_into().unwrap()) as usize;
        }
        pos = pos.saturating_add(12).saturating_add(len);
    }
    0
}

/// counts GIF image descriptors, stopping at `stop`. the flag says the walk ended (trailer,
/// `stop` or an unknown block) rather than running out of bytes.
fn gif_frames(data: &[u8], stop: usize) -> (usize, bool) {
    if data.len() < 13 {
        return (0, false);
    }
    let mut pos = 13usize;
    if data[10] & 0x80 != 0 {
        pos += 3 << ((data[10] & 0x07) + 1);
    }
    let mut count = 0;
    loop {
        if count >= stop {
            return (count, true);
        }
        let Some(&block) = data.get(pos) else { return (count, false) };
        match block {
            0x2c => {
                count += 1;
                let Some(&packed) = data.get(pos + 9) else { return (count, false) };
                pos += 10;
                if packed & 0x80 != 0 {
                    pos += 3 << ((packed & 0x07) + 1);
                }
                // LZW minimum code size, then the data sub-blocks.
                match skip_sub_blocks(data, pos + 1) {
                    Some(next) => pos = next,
                    None => return (count, false),
                }
            }
            0x21 => match skip_sub_blocks(data, pos + 2) {
                Some(next) => pos = next,
                None => return (count, false),
            },
            _ => return (count, true),
        }
    }
}

fn skip_sub_blocks(data: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let size = *data.get(pos)? as usize;
        pos += 1;
        if size == 0 {
            return Some(pos);
        }
        pos += size;
    }
}

/// an animated cover decoded at one display size, on disk. frames are straight-alpha RGBA,
/// `width` by `height`, played in order with `delays_ms`, looping forever.
#[derive(Clone, Debug)]
pub struct AnimatedArt {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub delays_ms: Vec<u32>,
    /// no transparent pixels in any frame.
    pub opaque: bool,
    frames: Vec<FrameEntry>,
}

#[derive(Clone, Debug)]
struct FrameEntry {
    codec: u8,
    offset: u64,
    len: u32,
}

impl AnimatedArt {
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// memory all frames take once decoded, at most `MAX_DECODED_BYTES`.
    pub fn decoded_bytes(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height) * 4 * self.frames.len() as u64
    }

    /// one frame, read and decoded from disk.
    pub fn frame(&self, index: usize) -> Result<RgbaImage> {
        let entry = self.frames.get(index).ok_or_else(|| Error::Sync(format!("animated cover has no frame {index}")))?;
        let mut file = std::fs::File::open(&self.path)?;
        file.seek(SeekFrom::Start(entry.offset))?;
        let mut data = vec![0; entry.len as usize];
        file.read_exact(&mut data)?;
        self.decode(entry.codec, &data)
    }

    /// every frame, decoded, from one read of the file.
    pub fn frames(&self) -> Result<Vec<RgbaImage>> {
        let file = std::fs::read(&self.path)?;
        self.frames
            .iter()
            .map(|e| {
                let range = e.offset as usize..e.offset as usize + e.len as usize;
                self.decode(e.codec, file.get(range).ok_or_else(|| corrupt("frame past the end"))?)
            })
            .collect()
    }

    fn decode(&self, codec: u8, data: &[u8]) -> Result<RgbaImage> {
        let format = if codec == CODEC_QOI { ImageFormat::Qoi } else { ImageFormat::Jpeg };
        let image = image::load_from_memory_with_format(data, format).map_err(|e| corrupt(&e.to_string()))?.into_rgba8();
        if image.dimensions() != (self.width, self.height) {
            return Err(corrupt("frame size"));
        }
        Ok(image)
    }

    /// the header and frame table of a cached file; `None` if it's missing.
    pub fn open(path: &Path) -> Result<Option<Self>> {
        let mut file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let file_len = file.metadata()?.len();
        let mut header = [0u8; HEADER_LEN];
        file.read_exact(&mut header).map_err(|_| corrupt("short header"))?;
        if &header[0..4] != MAGIC || header[4] != VERSION {
            return Err(corrupt("not a Navia animation"));
        }
        let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
        let (width, height, count) = (word(8), word(12), word(16) as usize);
        if width == 0 || height == 0 || !(2..=MAX_FRAMES).contains(&count) {
            return Err(corrupt("header"));
        }
        let mut table = vec![0u8; count * ENTRY_LEN];
        file.read_exact(&mut table).map_err(|_| corrupt("short table"))?;
        let mut offset = (HEADER_LEN + table.len()) as u64;
        let (mut delays_ms, mut frames) = (Vec::with_capacity(count), Vec::with_capacity(count));
        for entry in table.as_chunks::<ENTRY_LEN>().0 {
            let len = u32::from_le_bytes(entry[5..9].try_into().unwrap());
            delays_ms.push(u32::from_le_bytes(entry[0..4].try_into().unwrap()));
            frames.push(FrameEntry { codec: entry[4], offset, len });
            offset += u64::from(len);
        }
        if offset != file_len {
            return Err(corrupt("length"));
        }
        let opaque = frames.iter().all(|f| f.codec == CODEC_JPEG);
        Ok(Some(Self { path: path.to_path_buf(), width, height, delays_ms, opaque, frames }))
    }
}

fn corrupt(detail: &str) -> Error {
    Error::Decode { endpoint: "animated cover".to_string(), detail: detail.to_string() }
}

/// the output size and frame stride for a `src_w` by `src_h` source of `frames` frames shown at
/// `size` pixels, within `MAX_SIDE`, `MAX_FRAMES` and `MAX_DECODED_BYTES`.
fn plan(src_w: u32, src_h: u32, size: u32, frames: usize) -> (u32, u32, usize) {
    let src_long = src_w.max(src_h).max(1);
    let dims = |long: u32| {
        let scale = |v: u32| ((u64::from(v) * u64::from(long) + u64::from(src_long) / 2) / u64::from(src_long)).max(1) as u32;
        (scale(src_w), scale(src_h))
    };
    let bytes = |(w, h): (u32, u32), stride: usize| u64::from(w) * u64::from(h) * 4 * frames.div_ceil(stride) as u64;
    let mut long = size.max(1).min(src_long).min(MAX_SIDE);
    let mut stride = frames.div_ceil(MAX_FRAMES).max(1);
    let over = bytes(dims(long), stride);
    if over > MAX_DECODED_BYTES {
        let shrink = (MAX_DECODED_BYTES as f64 / over as f64).sqrt();
        long = ((f64::from(long) * shrink) as u32).max(MIN_SIDE.min(long));
        while bytes(dims(long), stride) > MAX_DECODED_BYTES && stride < frames {
            stride += 1;
        }
    }
    let (w, h) = dims(long);
    (w, h, stride)
}

fn delay_ms(delay: Delay) -> u32 {
    let (numer, denom) = delay.numer_denom_ms();
    let ms = (u64::from(numer) + u64::from(denom.max(1)) / 2) / u64::from(denom.max(1));
    let ms = ms.min(u64::from(u32::MAX / 4)) as u32;
    if ms <= FAST_DELAY_MS { FAST_DELAY_AS_MS } else { ms }
}

/// decode `source` (an animated GIF, WebP or PNG) for display at `size` pixels and write the
/// frames to `dest`. `None` if it turns out to have fewer than two frames.
pub fn decode_to_file(source: &[u8], size: u32, dest: &Path) -> Result<Option<AnimatedArt>> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(corrupt("source too large"));
    }
    let source_frames = frame_count(source).min(MAX_SOURCE_FRAMES);
    if source_frames < 2 {
        return Ok(None);
    }
    let bad = |e: image::ImageError| corrupt(&e.to_string());
    let cursor = Cursor::new(source);
    let (dims, frames) = if source.starts_with(b"GIF8") {
        let decoder = GifDecoder::new(cursor).map_err(bad)?;
        (checked(decoder.dimensions())?, decoder.into_frames())
    } else if source.starts_with(b"RIFF") {
        let decoder = WebPDecoder::new(cursor).map_err(bad)?;
        (checked(decoder.dimensions())?, decoder.into_frames())
    } else {
        let decoder = PngDecoder::new(cursor).map_err(bad)?;
        (checked(decoder.dimensions())?, decoder.apng().map_err(bad)?.into_frames())
    };
    let (width, height, stride) = plan(dims.0, dims.1, size, source_frames);

    // each kept frame: (delay, codec, encoded bytes). only the frame being read is full size.
    let mut kept: Vec<(u32, u8, Vec<u8>)> = Vec::new();
    for (index, frame) in frames.take(MAX_SOURCE_FRAMES).enumerate() {
        let frame = match frame {
            Ok(f) => f,
            // a damaged tail still leaves a playable loop.
            Err(_) if kept.len() >= 2 => break,
            Err(e) => return Err(bad(e)),
        };
        let delay = delay_ms(frame.delay());
        if index % stride != 0 {
            if let Some(last) = kept.last_mut() {
                last.0 = last.0.saturating_add(delay);
            }
            continue;
        }
        let full = frame.into_buffer();
        let small = if full.dimensions() == (width, height) { full } else { super::resize_rgba(&full, width, height, FilterType::CatmullRom) };
        let (codec, bytes) = encode_frame(&small)?;
        kept.push((delay, codec, bytes));
    }
    if kept.len() < 2 {
        return Ok(None);
    }

    let mut out = Vec::with_capacity(HEADER_LEN + kept.len() * ENTRY_LEN + kept.iter().map(|k| k.2.len()).sum::<usize>());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&[VERSION, 0, 0, 0]);
    for word in [width, height, kept.len() as u32] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    for (delay, codec, bytes) in &kept {
        out.extend_from_slice(&delay.to_le_bytes());
        out.push(*codec);
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    }
    for (_, _, bytes) in &kept {
        out.extend_from_slice(bytes);
    }
    super::atomic_write(dest, &out)?;
    AnimatedArt::open(dest)
}

fn checked((w, h): (u32, u32)) -> Result<(u32, u32)> {
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_SOURCE_PIXELS {
        return Err(corrupt("canvas size"));
    }
    Ok((w, h))
}

/// opaque frames as JPEG (small), frames with transparency as QOI (lossless, fast to decode).
fn encode_frame(frame: &RgbaImage) -> Result<(u8, Vec<u8>)> {
    let (w, h) = frame.dimensions();
    let mut bytes = Vec::new();
    let bad = |e: image::ImageError| corrupt(&e.to_string());
    if frame.pixels().all(|p| p.0[3] == 255) {
        let rgb: Vec<u8> = frame.as_raw().as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
        JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY).write_image(&rgb, w, h, ExtendedColorType::Rgb8).map_err(bad)?;
        Ok((CODEC_JPEG, bytes))
    } else {
        QoiEncoder::new(&mut bytes).write_image(frame.as_raw(), w, h, ExtendedColorType::Rgba8).map_err(bad)?;
        Ok((CODEC_QOI, bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::{Frame, Rgba};

    /// a `w` by `h` GIF of `n` solid frames in shifting colors, `delay_ms` apart.
    fn gif(w: u32, h: u32, n: usize, delay_ms: u32, transparent: bool) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = GifEncoder::new_with_speed(&mut out, 30);
            encoder.set_repeat(Repeat::Infinite).unwrap();
            for i in 0..n {
                let color = Rgba([(i * 40 % 256) as u8, 200 - (i * 20 % 200) as u8, 60, 255]);
                let image = RgbaImage::from_fn(w, h, |x, _| if transparent && x == 0 { Rgba([0, 0, 0, 0]) } else { color });
                encoder.encode_frame(Frame::from_parts(image, 0, 0, Delay::from_numer_denom_ms(delay_ms, 1))).unwrap();
            }
        }
        out
    }

    #[test]
    fn sniff_tells_animated_gifs_from_still_ones() {
        let animated = gif(8, 8, 3, 50, false);
        let still = gif(8, 8, 1, 0, false);
        assert_eq!(sniff(&animated), Sniff::Animated);
        assert_eq!(sniff(&still), Sniff::Still);
        assert_eq!(frame_count(&animated), 3);
        // the first frame alone can't tell yet.
        assert_eq!(sniff(&animated[..20]), Sniff::NeedMore);
        assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0xe0, 0, 16, b'J', b'F', b'I', b'F', 0, 1]), Sniff::Still);
    }

    #[test]
    fn sniff_reads_webp_and_png_headers() {
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0".to_vec();
        webp.extend_from_slice(&[0x12, 0, 0, 0]);
        assert_eq!(sniff(&webp), Sniff::Animated);
        webp[20] = 0x10;
        assert_eq!(sniff(&webp), Sniff::Still);
        let mut png = Vec::new();
        RgbaImage::new(4, 4).write_to(&mut Cursor::new(&mut png), ImageFormat::Png).unwrap();
        assert_eq!(sniff(&png), Sniff::Still);
    }

    #[test]
    fn decodes_once_and_reads_back_frames_and_delays() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.anim");
        let art = decode_to_file(&gif(40, 20, 4, 70, false), 20, &path).unwrap().unwrap();
        assert_eq!((art.width, art.height, art.frame_count()), (20, 10, 4));
        assert_eq!(art.delays_ms, vec![70; 4]);
        assert!(art.opaque);
        let reopened = AnimatedArt::open(&path).unwrap().unwrap();
        let frames = reopened.frames().unwrap();
        assert_eq!(frames.len(), 4);
        // JPEG keeps solid colors within a few steps.
        let px = frames[1].get_pixel(5, 5).0;
        assert!(px[0].abs_diff(40) < 6 && px[1].abs_diff(180) < 6, "{px:?}");
        assert_eq!(reopened.frame(3).unwrap().dimensions(), (20, 10));
    }

    #[test]
    fn transparency_is_kept_and_fast_delays_play_like_browsers() {
        let dir = tempfile::tempdir().unwrap();
        let art = decode_to_file(&gif(8, 8, 2, 0, true), 64, &dir.path().join("t.anim")).unwrap().unwrap();
        // never upscaled.
        assert_eq!((art.width, art.height), (8, 8));
        assert!(!art.opaque);
        assert_eq!(art.frame(0).unwrap().get_pixel(0, 3).0[3], 0);
        assert_eq!(art.delays_ms, vec![FAST_DELAY_AS_MS; 2]);
    }

    #[test]
    fn a_single_frame_gif_is_not_animated() {
        let dir = tempfile::tempdir().unwrap();
        assert!(decode_to_file(&gif(8, 8, 1, 0, false), 64, &dir.path().join("s.anim")).unwrap().is_none());
    }

    #[test]
    fn long_animations_merge_frames_and_keep_the_timing() {
        let dir = tempfile::tempdir().unwrap();
        let n = MAX_FRAMES + 10;
        let art = decode_to_file(&gif(4, 4, n, 20, false), 4, &dir.path().join("l.anim")).unwrap().unwrap();
        assert_eq!(art.frame_count(), n.div_ceil(2));
        assert_eq!(art.delays_ms.iter().sum::<u32>(), 20 * n as u32);
    }

    #[test]
    fn plan_stays_within_the_memory_budget() {
        // 600 px, 200 frames: 288 MB at full size, so it shrinks.
        let (w, h, stride) = plan(600, 600, 600, 200);
        assert!(u64::from(w) * u64::from(h) * 4 * 200u64.div_ceil(stride as u64) <= MAX_DECODED_BYTES);
        assert!(w >= MIN_SIDE && stride == 1, "{w}x{h} every {stride}");
        // already small: unchanged and never upscaled.
        assert_eq!(plan(331, 498, 600, 9), (331, 498, 1));
        // huge canvas, many frames: capped side, then frames dropped.
        let (w, h, stride) = plan(4000, 2000, 2000, 2000);
        assert!(u64::from(w) * u64::from(h) * 4 * 2000u64.div_ceil(stride as u64) <= MAX_DECODED_BYTES);
        assert!(w >= MIN_SIDE && stride >= 9, "{w}x{h} every {stride}");
    }

    #[test]
    fn a_damaged_file_reads_as_corrupt_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.anim");
        std::fs::write(&path, b"NVAN\x01\0\0\0junk").unwrap();
        assert!(AnimatedArt::open(&path).is_err());
        assert!(AnimatedArt::open(&dir.path().join("missing.anim")).unwrap().is_none());
    }
}

#[cfg(test)]
mod file_check {
    /// `NAVIA_ANIM_FILE=<image>`: decodes a real file at 600 px and prints what was kept.
    #[test]
    #[ignore]
    fn decode_a_real_file() {
        let source = std::fs::read(std::env::var("NAVIA_ANIM_FILE").unwrap()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        let art = super::decode_to_file(&source, 600, &dir.path().join("f.anim")).unwrap().expect("animated");
        println!(
            "{:?}, {} source frames: {}x{} {} frames, {} ms loop, file {} KB, {:?}",
            super::sniff(&source),
            super::frame_count(&source),
            art.width,
            art.height,
            art.frame_count(),
            art.delays_ms.iter().sum::<u32>(),
            std::fs::metadata(&art.path).unwrap().len() / 1024,
            start.elapsed()
        );
    }
}
