//! the small print at the end of an album page: the songs'
//! `copyright` tags, else the album's record labels, else nothing. only Navidrome's native API
//! sends the tag ([`crate::client::Client::album_copyrights`]); the labels are OpenSubsonic
//! `recordLabels` on the stored album, so other servers get the labels or nothing.

use std::collections::HashMap;

use super::Library;
use crate::client::Client;
use crate::error::Result;

/// most lines shown. a compilation can carry a different line on every song; the most common
/// ones say whose album it is, and a page of small print says nothing more.
pub const COPYRIGHT_LINES_MAX: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlbumFooterSource {
    Copyright,
    RecordLabel,
}

/// the lines to show, each one row of small print, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlbumFooter {
    pub lines: Vec<String>,
    pub source: AlbumFooterSource,
}

/// copyright lines from the songs' tag values (repeats count towards the order), else the
/// labels; `None` when there's neither.
pub fn footer(copyrights: &[String], labels: &[String]) -> Option<AlbumFooter> {
    let lines = copyright_lines(copyrights);
    if !lines.is_empty() {
        return Some(AlbumFooter { lines, source: AlbumFooterSource::Copyright });
    }
    let lines = distinct(labels.iter().map(|l| tidy(l)));
    (!lines.is_empty()).then_some(AlbumFooter { lines, source: AlbumFooterSource::RecordLabel })
}

/// distinct tag values, most songs first, then in track order, at most [`COPYRIGHT_LINES_MAX`].
fn copyright_lines(values: &[String]) -> Vec<String> {
    let mut counts: HashMap<String, (usize, usize)> = HashMap::new();
    let mut lines: Vec<String> = Vec::new();
    for value in values.iter().map(|v| marks(&tidy(v))).filter(|v| !v.is_empty()) {
        let key = value.to_lowercase();
        let first = lines.len();
        let entry = counts.entry(key).or_insert_with(|| {
            lines.push(value);
            (0, first)
        });
        entry.0 += 1;
    }
    let rank = |line: &String| counts[&line.to_lowercase()];
    lines.sort_by(|a, b| {
        let ((ca, fa), (cb, fb)) = (rank(a), rank(b));
        cb.cmp(&ca).then(fa.cmp(&fb))
    });
    lines.truncate(COPYRIGHT_LINES_MAX);
    lines
}

fn tidy(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// "(C)" and "(P)" written as words become © and ℗, as tags typed without the symbols often have them.
fn marks(text: &str) -> String {
    text.split(' ')
        .map(|word| match word {
            "(C)" | "(c)" => "©",
            "(P)" | "(p)" => "℗",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn distinct(items: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items.filter(|i| !i.is_empty()) {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&item)) {
            out.push(item);
        }
    }
    out
}

/// the album's footer: the server's copyright tags when `use_server` allows asking (it logs in,
/// see `client::native`, and [`super::discography::server_list_allowed`]) and the server has
/// them, else the stored album's labels. errors only when the library can't be read.
pub async fn album_footer(client: &Client, library: &Library, album_id: &str, use_server: bool) -> Result<Option<AlbumFooter>> {
    let copyrights = if use_server {
        client.album_copyrights(album_id).await.unwrap_or_else(|e| {
            if !matches!(e, crate::error::Error::HttpStatus { status: 404, .. }) {
                eprintln!("navia: couldn't read an album's copyright tags, using its labels: {e}");
            }
            Vec::new()
        })
    } else {
        Vec::new()
    };
    let labels: Vec<String> = library.album(album_id)?.map(|a| a.record_labels.into_iter().map(|l| l.name).collect()).unwrap_or_default();
    Ok(footer(&copyrights, &labels))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|i| i.to_string()).collect()
    }

    #[test]
    fn copyright_wins_over_labels_and_neither_hides() {
        let f = footer(&s(&["℗ 2009 Calderstone"]), &s(&["EMI"])).unwrap();
        assert_eq!((f.lines, f.source), (s(&["℗ 2009 Calderstone"]), AlbumFooterSource::Copyright));
        let f = footer(&s(&["  ", ""]), &s(&["EMI", " emi ", "Parlophone"])).unwrap();
        assert_eq!((f.lines, f.source), (s(&["EMI", "Parlophone"]), AlbumFooterSource::RecordLabel));
        assert!(footer(&[], &[]).is_none());
        assert!(footer(&s(&[" "]), &s(&[""])).is_none());
    }

    #[test]
    fn most_common_first_then_track_order_and_capped() {
        let values = s(&["© 2020 B", "℗ 2020 A", "℗ 2020 A", "© 2020  b", "D", "E", "C"]);
        assert_eq!(footer(&values, &[]).unwrap().lines, s(&["© 2020 B", "℗ 2020 A", "D"]));
    }

    #[test]
    fn written_marks_become_symbols_only_as_words() {
        let f = footer(&s(&["(C) 2011 Play Digital (P) 2011 Play Digital", "Label (Pty) (c)"]), &[]).unwrap();
        assert_eq!(f.lines, s(&["© 2011 Play Digital ℗ 2011 Play Digital", "Label (Pty) ©"]));
    }
}
