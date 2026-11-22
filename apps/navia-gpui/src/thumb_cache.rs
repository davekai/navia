//! decoded thumbnails, bounded by bytes; evicted ones also leave the sprite atlas, which otherwise keeps them all.

use std::collections::HashMap;
use std::sync::Arc;

use std::time::{Duration, Instant};

use gpui::{RenderImage, SharedString, Window};

pub const THUMB_CACHE_BYTES: usize = 64 * 1024 * 1024;

pub type ThumbKey = (SharedString, u32);

pub struct ThumbEntry {
    pub image: Option<Arc<RenderImage>>,
    pub loading: bool,
    pub last_used: u64,
    pub bytes: usize,
    /// When the image arrived, while it fades in over its placeholder.
    pub arrived: Option<Instant>,
    /// When to ask again after a failed load. Failures are never cached for good, because they
    /// can be a network blip or a busy server.
    pub retry_at: Option<Instant>,
    /// Failures in a row, for the backoff.
    pub attempts: u32,
    /// for the server's no-cover placeholder: when to ask again, since a cover may have been added.
    pub expires_at: Option<Instant>,
}

#[derive(Default)]
pub struct ThumbCache {
    map: HashMap<ThumbKey, ThumbEntry>,
    bytes: usize,
}

impl ThumbCache {
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ThumbKey, &ThumbEntry)> {
        self.map.iter()
    }

    pub fn get_mut(&mut self, key: &ThumbKey) -> Option<&mut ThumbEntry> {
        self.map.get_mut(key)
    }

    pub fn insert_loading(&mut self, key: ThumbKey, frame: u64) {
        self.map.insert(key, ThumbEntry { image: None, loading: true, last_used: frame, bytes: 0, arrived: None, retry_at: None, attempts: 0, expires_at: None });
    }

    pub fn remove(&mut self, key: &ThumbKey) {
        if let Some(entry) = self.map.remove(key) {
            self.bytes -= entry.bytes;
        }
    }

    /// Drops every decoded thumbnail while the window is hidden. Loads in flight stay wanted.
    pub fn release_images(&mut self) -> Vec<Arc<RenderImage>> {
        let decoded: Vec<ThumbKey> = self.map.iter().filter(|(_, e)| !e.loading).map(|(k, _)| k.clone()).collect();
        let mut images = Vec::new();
        for key in decoded {
            if let Some(entry) = self.map.remove(&key) {
                self.bytes -= entry.bytes;
                images.extend(entry.image);
            }
        }
        images
    }

    /// Forgets every size of this key, so the next draw asks for it again.
    pub fn forget(&mut self, key: &str) {
        let stale: Vec<ThumbKey> = self.map.keys().filter(|(k, _)| k.as_ref() == key).cloned().collect();
        for key in stale {
            self.remove(&key);
        }
    }

    /// stores a decoded image for a key that's still wanted. a `placeholder` expires after the
    /// core's own `NO_STORE_TTL`.
    pub fn set_image(&mut self, key: &ThumbKey, image: Option<Arc<RenderImage>>, placeholder: bool) {
        if let Some(entry) = self.map.get_mut(key) {
            let bytes = image.as_ref().map_or(0, image_bytes);
            self.bytes = self.bytes - entry.bytes + bytes;
            entry.bytes = bytes;
            // a refreshed image that's the same placeholder again doesn't fade in a second time.
            let again = placeholder && entry.image.is_some();
            entry.arrived = (image.is_some() && !again).then(Instant::now);
            entry.expires_at = (placeholder && image.is_some()).then(|| Instant::now() + navia_core::artwork::NO_STORE_TTL);
            if image.is_some() {
                entry.retry_at = None;
                entry.attempts = 0;
            } else {
                entry.retry_at = Some(Instant::now() + retry_delay(entry.attempts));
                entry.attempts = entry.attempts.saturating_add(1);
            }
            entry.image = image;
            entry.loading = false;
        }
    }

    /// Evicts least recently used entries once over budget, but never one drawn in the last two
    /// frames, so a full screen can't churn.
    pub fn evict(&mut self, frame: u64, window: &mut Window) {
        // Failed lookups cost an entry but no bytes, so bound those too.
        const MAX_ENTRIES: usize = 20_000;
        if self.bytes <= THUMB_CACHE_BYTES && self.map.len() <= MAX_ENTRIES {
            return;
        }
        let mut stale: Vec<(ThumbKey, u64)> = self.map.iter().filter(|(_, e)| !e.loading && e.last_used + 2 < frame).map(|(k, e)| (k.clone(), e.last_used)).collect();
        stale.sort_unstable_by_key(|(_, used)| *used);
        let target_bytes = THUMB_CACHE_BYTES * 3 / 4;
        for (key, _) in stale {
            if self.bytes <= target_bytes && self.map.len() <= MAX_ENTRIES * 3 / 4 {
                break;
            }
            if let Some(entry) = self.map.remove(&key) {
                self.bytes -= entry.bytes;
                if let Some(image) = entry.image {
                    let _ = window.drop_image(image);
                }
            }
        }
    }
}

/// 2, 4, 8 and so on seconds, capped at a minute.
fn retry_delay(attempts: u32) -> Duration {
    Duration::from_secs((2u64 << attempts.min(6)).min(60))
}

fn image_bytes(image: &Arc<RenderImage>) -> usize {
    (0..image.frame_count()).map(|i| image.as_bytes(i).map_or(0, |b| b.len())).sum()
}
