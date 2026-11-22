//! Song lists by row: a list's order as cheap row ids, and the rows on demand, so a UI's memory
//! doesn't grow with the library.

use rusqlite::params_from_iter;

use super::Library;
use super::text::fts_query;
use crate::client::Song;
use super::browse::RankedArtist;
use crate::error::{Error, Result};

fn parse<T: serde::de::DeserializeOwned>(data: &str) -> Result<T> {
    serde_json::from_str(data).map_err(|e| Error::Sync(e.to_string()))
}

/// The order of a song list. Each has an index, so a full list never sorts the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SongOrder {
    MostPlayed,
    Title,
    Artist,
    Album,
    Year,
    Duration,
    /// Newest first, by the server's `created` date.
    Added,
}

impl SongOrder {
    pub(crate) fn clause(self) -> &'static str {
        match self {
            SongOrder::MostPlayed => "s.play_count DESC, s.sort_name",
            SongOrder::Title => "s.sort_name",
            SongOrder::Artist => "lower(coalesce(json_extract(s.data, '$.displayArtist'), json_extract(s.data, '$.artist'), '')), s.sort_name",
            SongOrder::Album => "lower(coalesce(json_extract(s.data, '$.album'), '')), s.album_id, s.disc, s.track",
            SongOrder::Year => "s.year DESC, s.sort_name",
            SongOrder::Duration => "s.duration DESC, s.sort_name",
            SongOrder::Added => "s.created DESC, s.sort_name",
        }
    }
}

impl Library {
    /// Row ids of every song in order, filtered by a search string when given. Rows come from
    /// [`Self::songs_by_rowid`].
    pub fn song_order(&self, order: SongOrder, filter: Option<&str>) -> Result<Vec<u32>> {
        let clause = order.clause();
        let fts = filter.filter(|f| !f.trim().is_empty()).and_then(fts_query);
        self.read(|c| {
            let mut out = Vec::new();
            if let Some(fts) = &fts {
                let sql = format!("SELECT s.rowid FROM fts_song f JOIN song s ON s.rowid = f.rowid WHERE fts_song MATCH ?1 ORDER BY {clause}");
                let mut st = c.prepare_cached(&sql)?;
                let rows = st.query_map([fts], |r| r.get::<_, i64>(0))?;
                for row in rows {
                    out.push(row? as u32);
                }
            } else {
                let sql = format!("SELECT s.rowid FROM song s ORDER BY {clause}");
                let mut st = c.prepare_cached(&sql)?;
                let rows = st.query_map([], |r| r.get::<_, i64>(0))?;
                for row in rows {
                    out.push(row? as u32);
                }
            }
            Ok(out)
        })
    }

    /// Songs whose title has every word of `words` as a prefix, at most `limit`, most played
    /// first. Candidates for [`crate::lanyard::matching`].
    pub fn songs_titled(&self, words: &str, limit: u32) -> Result<Vec<(u32, Song)>> {
        let Some(fts) = fts_query(words) else { return Ok(Vec::new()) };
        let rowids = self.read(|c| {
            let mut st = c.prepare_cached("SELECT s.rowid FROM fts_song f JOIN song s ON s.rowid = f.rowid WHERE fts_song MATCH ?1 ORDER BY s.play_count DESC, s.rowid LIMIT ?2")?;
            let rows = st.query_map(rusqlite::params![format!("title : ({fts})"), limit], |r| r.get::<_, i64>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row? as u32);
            }
            Ok(out)
        })?;
        self.songs_by_rowid(&rowids)
    }

    /// Songs carrying any of these ISRCs, most played first, each with the ISRC it matched.
    /// This is a full scan with no index, so ask for a whole batch at once.
    pub fn songs_with_isrc(&self, isrcs: &[String]) -> Result<Vec<(u32, Song, String)>> {
        let wanted: Vec<String> = isrcs.iter().map(|i| i.trim().to_ascii_uppercase()).filter(|i| !i.is_empty()).collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let found = self.read(|c| {
            let holders = vec!["?"; wanted.len()].join(",");
            let sql = format!(
                "SELECT s.rowid, s.data, upper(j.value) FROM song s, json_each(s.data, '$.isrc') j \
                 WHERE upper(j.value) IN ({holders}) ORDER BY s.play_count DESC, s.rowid"
            );
            let mut st = c.prepare(&sql)?;
            let rows = st.query_map(params_from_iter(wanted.iter()), |r| Ok((r.get::<_, i64>(0)? as u32, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })?;
        let mut out = Vec::with_capacity(found.len());
        for (rowid, data, isrc) in found {
            let song: Song = serde_json::from_str(&data).map_err(|e| Error::Sync(e.to_string()))?;
            out.push((rowid, song, isrc));
        }
        Ok(out)
    }

    /// songs carrying any of these recording MBIDs (`musicBrainzId` on the stored song), most played
    /// first, each with the MBID it matched in lowercase. one pass over the songs, like
    /// [`Library::songs_with_isrc`].
    pub fn songs_with_mbid(&self, mbids: &[String]) -> Result<Vec<(u32, Song, String)>> {
        let wanted: Vec<String> = mbids.iter().map(|m| m.trim().to_ascii_lowercase()).filter(|m| !m.is_empty()).collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let found = self.read(|c| {
            let holders = vec!["?"; wanted.len()].join(",");
            let sql = format!(
                "SELECT rowid, data, lower(json_extract(data, '$.musicBrainzId')) AS m FROM song \
                 WHERE m IN ({holders}) ORDER BY play_count DESC, rowid"
            );
            let mut st = c.prepare(&sql)?;
            let rows = st.query_map(params_from_iter(wanted.iter()), |r| Ok((r.get::<_, i64>(0)? as u32, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })?;
        let mut out = Vec::with_capacity(found.len());
        for (rowid, data, mbid) in found {
            let song: Song = serde_json::from_str(&data).map_err(|e| Error::Sync(e.to_string()))?;
            out.push((rowid, song, mbid));
        }
        Ok(out)
    }

    /// What the Songs filters read about every song, read once and kept until the library is written to again. Reads
    /// in chunks, so other readers wait at most one chunk.
    pub fn song_filters(&self) -> Result<std::sync::Arc<crate::song_filters::FilterIndex>> {
        let version = self.version();
        if let Some((v, index)) = self.inner.song_filters.lock().expect("song filters cache").as_ref()
            && *v == version
        {
            return Ok(index.clone());
        }
        let mut index = crate::song_filters::FilterIndex::default();
        let mut after = -1i64;
        loop {
            let chunk: Vec<(i64, String)> = self.read(|c| {
                let mut st = c.prepare_cached("SELECT rowid, data FROM song WHERE rowid > ?1 ORDER BY rowid LIMIT 2000")?;
                let rows = st.query_map([after], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })?;
            let Some(last) = chunk.last() else { break };
            after = last.0;
            for (rowid, data) in &chunk {
                let song: Song = serde_json::from_str(data).map_err(|e| Error::Sync(e.to_string()))?;
                index.add(*rowid as u32, &song);
            }
        }
        let index = std::sync::Arc::new(index);
        *self.inner.song_filters.lock().expect("song filters cache") = Some((version, index.clone()));
        Ok(index)
    }

    /// What the Artists filters read about every artist, cached until the library is written to.
    /// Songs are read in chunks.
    pub fn artist_filters(&self) -> Result<std::sync::Arc<crate::artist_filters::ArtistFilterIndex>> {
        let version = self.version();
        if let Some((v, index)) = self.inner.artist_filters.lock().expect("artist filters cache").as_ref()
            && *v == version
        {
            return Ok(index.clone());
        }
        let mut b = crate::artist_filters::ArtistFilterBuilder::default();
        for table in ["artist", "album"] {
            let mut after = -1i64;
            loop {
                let chunk: Vec<(i64, String)> = self.read(|c| {
                    let mut st = c.prepare_cached(&format!("SELECT rowid, data FROM {table} WHERE rowid > ?1 ORDER BY rowid LIMIT 2000"))?;
                    let rows = st.query_map([after], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
                    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
                })?;
                let Some(last) = chunk.last() else { break };
                after = last.0;
                for (_, data) in &chunk {
                    if table == "artist" {
                        b.add_artist(&parse::<crate::client::Artist>(data)?);
                    } else {
                        b.add_album(&parse::<crate::client::Album>(data)?);
                    }
                }
            }
        }
        let mut after = -1i64;
        loop {
            let chunk: Vec<(i64, String)> = self.read(|c| {
                let mut st = c.prepare_cached("SELECT rowid, data FROM song WHERE rowid > ?1 ORDER BY rowid LIMIT 2000")?;
                let rows = st.query_map([after], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })?;
            let Some(last) = chunk.last() else { break };
            after = last.0;
            for (rowid, data) in &chunk {
                b.add_song(*rowid as u32, &parse::<crate::artist_filters::SongCredits>(data)?);
            }
        }
        let index = std::sync::Arc::new(b.finish());
        *self.inner.artist_filters.lock().expect("artist filters cache") = Some((version, index.clone()));
        Ok(index)
    }

    /// The Artists page, narrowed by the search text and the filters. Date Added counts back from
    /// `now_ms`.
    pub fn filtered_artists(&self, query: Option<&str>, filters: &crate::song_filters::FilterState, now_ms: u64) -> Result<Vec<RankedArtist>> {
        let mut artists = self.ranked_artists(query, None)?;
        if filters.is_empty() {
            return Ok(artists);
        }
        let index = self.artist_filters()?;
        let downloaded: std::collections::HashSet<u32> = if filters.is_set(crate::song_filters::FilterId::Downloaded) { self.downloaded_rowids()?.into_iter().collect() } else { Default::default() };
        let keep = index.matching(filters, &crate::song_filters::MatchContext { downloaded: &downloaded, now_ms });
        artists.retain(|a| keep.contains(&a.artist.id));
        Ok(artists)
    }

    /// The Songs list's row ids in order, narrowed by the search text and the filters. Date
    /// filters count back from `now_ms`.
    pub fn filtered_song_order(&self, order: SongOrder, query: Option<&str>, filters: &crate::song_filters::FilterState, now_ms: u64) -> Result<Vec<u32>> {
        let rowids = self.song_order(order, query)?;
        if filters.is_empty() {
            return Ok(rowids);
        }
        let index = self.song_filters()?;
        let downloaded: std::collections::HashSet<u32> = if filters.is_set(crate::song_filters::FilterId::Downloaded) { self.downloaded_rowids()?.into_iter().collect() } else { Default::default() };
        Ok(index.narrow(rowids, filters, &crate::song_filters::MatchContext { downloaded: &downloaded, now_ms }))
    }

    /// The songs for these row ids, in the order given. Unknown ids are left out.
    pub fn songs_by_rowid(&self, rowids: &[u32]) -> Result<Vec<(u32, Song)>> {
        let mut found = std::collections::HashMap::with_capacity(rowids.len());
        self.read(|c| {
            for chunk in rowids.chunks(500) {
                let holders = vec!["?"; chunk.len()].join(",");
                let sql = format!("SELECT rowid, data FROM song WHERE rowid IN ({holders})");
                let mut st = c.prepare_cached(&sql)?;
                let rows = st.query_map(params_from_iter(chunk.iter().map(|r| *r as i64)), |r| Ok((r.get::<_, i64>(0)? as u32, r.get::<_, String>(1)?)))?;
                for row in rows {
                    let (rowid, data) = row?;
                    let song: Song = serde_json::from_str(&data).map_err(|e| Error::Sync(e.to_string()))?;
                    found.insert(rowid, song);
                }
            }
            Ok(())
        })?;
        Ok(rowids.iter().filter_map(|r| found.remove(r).map(|s| (*r, s))).collect())
    }

    /// Fewer Repeats' recency for each row id, in the order given. Songs in `heard` score 1, others
    /// go by their last-played date, raised a little for recent skips. Unknown rows count as never played.
    pub fn recency_by_rowid(&self, rowids: &[u32], heard: &std::collections::HashSet<String>, now_ms: u64) -> Result<Vec<f32>> {
        let mut found: std::collections::HashMap<u32, (String, Option<String>)> = std::collections::HashMap::with_capacity(rowids.len());
        self.read(|c| {
            for chunk in rowids.chunks(500) {
                let holders = vec!["?"; chunk.len()].join(",");
                let mut st = c.prepare_cached(&format!("SELECT rowid, id, played FROM song WHERE rowid IN ({holders})"))?;
                let rows = st.query_map(params_from_iter(chunk.iter().map(|r| *r as i64)), |r| Ok((r.get::<_, i64>(0)? as u32, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?)))?;
                for row in rows {
                    let (rowid, id, played) = row?;
                    found.insert(rowid, (id, played));
                }
            }
            Ok(())
        })?;
        let tired = crate::engine::fatigue::load(self, now_ms as i64);
        Ok(rowids
            .iter()
            .map(|r| match found.get(r) {
                Some((id, played)) => crate::queue_order::recency(heard.contains(id), played.as_deref().and_then(crate::queue_sync::parse_iso8601_ms), now_ms).max(tired.get(id).map_or(0.0, |f| f.recency())),
                None => 0.0,
            })
            .collect())
    }

    /// [`Library::recency_by_rowid`] for song ids.
    pub fn recency_by_id(&self, ids: &[String], heard: &std::collections::HashSet<String>, now_ms: u64) -> Result<Vec<f32>> {
        let mut found: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::with_capacity(ids.len());
        self.read(|c| {
            let mut st = c.prepare_cached("SELECT played FROM song WHERE id = ?1")?;
            for id in ids {
                if !found.contains_key(id)
                    && let Some(played) = st.query_map([id], |r| r.get::<_, Option<String>>(0))?.next()
                {
                    found.insert(id.clone(), played?);
                }
            }
            Ok(())
        })?;
        let tired = crate::engine::fatigue::load(self, now_ms as i64);
        Ok(ids
            .iter()
            .map(|id| crate::queue_order::recency(heard.contains(id), found.get(id).cloned().flatten().as_deref().and_then(crate::queue_sync::parse_iso8601_ms), now_ms).max(tired.get(id).map_or(0.0, |f| f.recency())))
            .collect())
    }

    /// Row ids for song ids, in the order given. Unknown songs are left out.
    pub fn song_rowids(&self, ids: &[String]) -> Result<Vec<u32>> {
        self.read(|c| {
            let mut out = Vec::with_capacity(ids.len());
            let mut st = c.prepare_cached("SELECT rowid FROM song WHERE id = ?1")?;
            for id in ids {
                if let Some(rowid) = st.query_map([id], |r| r.get::<_, i64>(0))?.next() {
                    out.push(rowid? as u32);
                }
            }
            Ok(out)
        })
    }

    /// Song ids for row ids, in the order given, repeats kept. Unknown rows are left out.
    pub fn song_ids_by_rowid(&self, rowids: &[u32]) -> Result<Vec<String>> {
        let mut found: std::collections::HashMap<u32, String> = std::collections::HashMap::with_capacity(rowids.len());
        self.read(|c| {
            for chunk in rowids.chunks(500) {
                let holders = vec!["?"; chunk.len()].join(",");
                let mut st = c.prepare_cached(&format!("SELECT rowid, id FROM song WHERE rowid IN ({holders})"))?;
                let rows = st.query_map(params_from_iter(chunk.iter().map(|r| *r as i64)), |r| Ok((r.get::<_, i64>(0)? as u32, r.get::<_, String>(1)?)))?;
                for row in rows {
                    let (rowid, id) = row?;
                    found.insert(rowid, id);
                }
            }
            Ok(())
        })?;
        Ok(rowids.iter().filter_map(|r| found.get(r).cloned()).collect())
    }

    /// Durations in milliseconds for song ids. Unknown songs and songs without a duration are
    /// left out.
    pub fn durations_by_id(&self, ids: &[String]) -> Result<std::collections::HashMap<String, u64>> {
        let mut found = std::collections::HashMap::with_capacity(ids.len());
        self.read(|c| {
            for chunk in ids.chunks(500) {
                let holders = vec!["?"; chunk.len()].join(",");
                let mut st = c.prepare_cached(&format!("SELECT id, duration FROM song WHERE id IN ({holders})"))?;
                let rows = st.query_map(params_from_iter(chunk.iter()), |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?)))?;
                for row in rows {
                    let (id, duration) = row?;
                    if let Some(d) = duration.filter(|d| *d > 0) {
                        found.insert(id, d as u64 * 1000);
                    }
                }
            }
            Ok(())
        })?;
        Ok(found)
    }

    /// An album's songs with their row ids, in disc and track order.
    pub fn album_rows(&self, album_id: &str) -> Result<Vec<(u32, Song)>> {
        self.read(|c| {
            let mut st = c.prepare_cached("SELECT rowid, data FROM song WHERE album_id = ?1 ORDER BY disc, track, sort_name")?;
            let rows = st.query_map([album_id], |r| Ok((r.get::<_, i64>(0)? as u32, r.get::<_, String>(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                let (rowid, data) = row?;
                out.push((rowid, serde_json::from_str(&data).map_err(|e| Error::Sync(e.to_string()))?));
            }
            Ok(out)
        })
    }

    /// Row ids of an artist's album songs, newest album first. A fallback for when the server's
    /// top songs are empty.
    pub fn artist_album_rowids(&self, artist_id: &str) -> Result<Vec<u32>> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT s.rowid FROM song s JOIN album a ON a.id = s.album_id WHERE a.artist_id = ?1
                 ORDER BY a.year DESC, a.sort_name, s.disc, s.track",
            )?;
            let rows = st.query_map([artist_id], |r| r.get::<_, i64>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row? as u32);
            }
            Ok(out)
        })
    }

    /// Row ids of the songs this user has starred, newest first when the server gave dates.
    pub fn starred_song_rowids(&self) -> Result<Vec<u32>> {
        self.read(|c| {
            let mut st = c.prepare_cached("SELECT rowid FROM song WHERE starred IS NOT NULL ORDER BY starred DESC, rowid")?;
            let rows = st.query_map([], |r| r.get::<_, i64>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row? as u32);
            }
            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::MusicFolder;
    use crate::library::LibraryConfig;

    fn song(id: &str, title: &str, artist: &str, album: &str, plays: i64) -> Song {
        Song { id: id.into(), title: title.into(), artist: Some(artist.into()), album: Some(album.into()), album_id: Some(format!("al-{album}")), play_count: Some(plays), ..Default::default() }
    }

    #[test]
    fn order_filter_and_rows_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(&LibraryConfig { path: dir.path().join("lib.sqlite") }).unwrap();
        lib.replace_libraries(&[MusicFolder { id: "1".into(), name: "L".into() }]).unwrap();
        let cur = lib.begin_sync("1", None, false).unwrap();
        lib.apply_songs("1", &cur, 3, &[song("a", "Zebra", "Bob", "Alpha", 1), song("b", "Apple", "Ann", "Beta", 5), song("c", "Mango", "Cal", "Gamma", 3)]).unwrap();

        let by_plays = lib.song_order(SongOrder::MostPlayed, None).unwrap();
        let songs = lib.songs_by_rowid(&by_plays).unwrap();
        assert_eq!(songs.iter().map(|(_, s)| s.id.as_str()).collect::<Vec<_>>(), ["b", "c", "a"]);

        let by_artist = lib.song_order(SongOrder::Artist, None).unwrap();
        let songs = lib.songs_by_rowid(&by_artist).unwrap();
        assert_eq!(songs[0].1.artist.as_deref(), Some("Ann"));

        let filtered = lib.song_order(SongOrder::Title, Some("mang")).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(lib.songs_by_rowid(&filtered).unwrap()[0].1.id, "c");
        assert!(lib.song_order(SongOrder::Title, Some("   ")).unwrap().len() == 3, "blank filter is no filter");

        let rowids = lib.song_rowids(&["c".to_string(), "missing".to_string(), "a".to_string()]).unwrap();
        assert_eq!(rowids.len(), 2);
        assert_eq!(lib.songs_by_rowid(&rowids).unwrap()[0].1.id, "c");
    }

    #[test]
    fn queue_lookups_keep_order_and_repeats() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(&LibraryConfig { path: dir.path().join("lib.sqlite") }).unwrap();
        lib.replace_libraries(&[MusicFolder { id: "1".into(), name: "L".into() }]).unwrap();
        let cur = lib.begin_sync("1", None, false).unwrap();
        let timed = |id: &str, secs: Option<i64>| Song { duration: secs, ..song(id, id, "A", "B", 0) };
        lib.apply_songs("1", &cur, 3, &[timed("a", Some(200)), timed("b", None), timed("c", Some(61))]).unwrap();
        let rowids = lib.song_rowids(&["c".to_string(), "a".to_string()]).unwrap();
        let ids = lib.song_ids_by_rowid(&[rowids[0], 999_999, rowids[1], rowids[0]]).unwrap();
        assert_eq!(ids, ["c", "a", "c"], "unknown rows are left out, repeats kept");
        let durations = lib.durations_by_id(&["a".to_string(), "b".to_string(), "c".to_string(), "x".to_string()]).unwrap();
        assert_eq!(durations.get("a"), Some(&200_000));
        assert_eq!(durations.get("c"), Some(&61_000));
        assert!(!durations.contains_key("b") && !durations.contains_key("x"));
    }
}
