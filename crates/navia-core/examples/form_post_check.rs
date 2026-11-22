//! reads one album, its cover and a few seconds of one song's stream with a seek, read-only, to
//! check how requests go out on a real server. point `NAVIDROME_URL` at a logging proxy to see
//! each request's method and where its parameters were. nothing plays and nothing is written.
//!
//! `NAVIDROME_URL=... NAVIDROME_USER=... NAVIDROME_PASSWORD=... cargo run --release -p navia-core
//! --example form_post_check`

use std::io::{Read, Seek, SeekFrom};

use navia_core::artwork::{ArtworkCache, ArtworkConfig};
use navia_core::audio::stream::{HttpFetcher, HttpStream, StreamConfig};
use navia_core::client::{AlbumListOptions, Client, ClientConfig, Credentials, StreamOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let env = |k: &str| std::env::var(k).map_err(|_| format!("{k} is not set"));
    let cfg = ClientConfig::new(env("NAVIDROME_URL")?, Credentials::new(env("NAVIDROME_USER")?, env("NAVIDROME_PASSWORD")?)).read_only();
    let client = Client::new(cfg)?;
    client.ping().await?;
    println!("formPost: {:?}", client.form_post().known());

    let album = client.album_list2(&AlbumListOptions::new("newest", 1, 0)).await?.into_iter().next().ok_or("no albums")?;
    let album = client.album(&album.id).await?;
    let song = album.song.first().ok_or("an empty album")?.clone();
    println!("album: {} songs", album.song.len());

    let dir = tempfile::tempdir()?;
    let artwork = ArtworkCache::new(client.clone(), ArtworkConfig { dir: dir.path().to_path_buf(), max_bytes: u64::MAX })?;
    let cover = artwork.cover(album.cover_art.as_deref().unwrap_or(&album.id), Some(300)).await?;
    println!("cover: {}x{}", cover.width, cover.height);

    let fetcher = HttpFetcher::new()?.with_form_post(client.form_post());
    let url = client.stream_url(&song.id, &StreamOptions { format: Some("raw".to_string()), ..Default::default() })?;
    let report = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let stream = HttpStream::open(&fetcher, url, StreamConfig { read_ahead: 512 * 1024, ..Default::default() }).map_err(|e| e.to_string())?;
        let mut reader = stream.reader();
        let mut head = vec![0u8; 64 * 1024];
        reader.read_exact(&mut head).map_err(|e| e.to_string())?;
        let total = reader.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
        reader.seek(SeekFrom::Start(total / 2)).map_err(|e| e.to_string())?;
        let mut middle = vec![0u8; 512 * 1024];
        reader.read_exact(&mut middle).map_err(|e| e.to_string())?;
        let stats = stream.stats();
        Ok(format!("stream: seekable {}, {total} bytes, read the head and 512 KB from the middle, {stats:?}", stream.is_seekable()))
    })
    .await??;
    println!("{report}");
    Ok(())
}
