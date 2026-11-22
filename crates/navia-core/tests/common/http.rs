//! A minimal local HTTP server with byte-range support, standing in for Navidrome's `/rest/stream`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Req {
    pub id: String,
    pub format: Option<String>,
    pub time_offset: Option<u64>,
    pub max_bit_rate: Option<u32>,
    pub range: Option<String>,
    /// The endpoint, like `stream`.
    pub endpoint: String,
    pub method: String,
    /// the parameter names in the URL and in a form body.
    pub query_keys: Vec<String>,
    pub body_keys: Vec<String>,
}

#[derive(Default)]
pub struct Library {
    /// id -> raw file bytes.
    pub raw: HashMap<String, Vec<u8>>,
    /// (id, format) -> transcoded bytes. Served without ranges and with no length.
    pub transcoded: HashMap<(String, String), Vec<u8>>,
    /// id -> `Last-Modified` sent with the raw file, if any.
    pub stamps: HashMap<String, String>,
}

#[derive(Clone, Copy, Default)]
pub struct Faults {
    /// Serves raw files without range support, as a 200 with the whole body.
    pub no_ranges: bool,
    /// Fail this many range requests with a 500 first.
    pub fail_first: u32,
    /// Limits transfers to this many bytes per second, 0 for unlimited.
    pub throttle: u64,
    /// Stall this long before responding to a range request.
    pub stall: Duration,
    /// Send this content type for transcodes, as a server substituting a format would.
    pub transcode_mime: Option<&'static str>,
}

/// What the fake says to `getTranscodeDecision`, for a server with the `transcoding` extension.
#[derive(Clone, Debug, Default)]
pub struct FakeExtension {
    pub can_direct_play: bool,
    pub can_transcode: bool,
}

pub struct ServerState {
    pub library: Mutex<Library>,
    pub faults: Mutex<Faults>,
    pub requests: Mutex<Vec<Req>>,
    pub extension: Mutex<Option<FakeExtension>>,
    /// Bodies of `getTranscodeDecision` requests.
    pub decisions: Mutex<Vec<String>>,
    failures: Mutex<u32>,
    /// While set, connections are dropped as if the server were gone.
    pub down: AtomicBool,
    /// every connection and request that reached the fake, whatever its endpoint.
    pub seen: std::sync::atomic::AtomicUsize,
    /// list `formPost`, and read parameters from a POST form too.
    pub form_post: AtomicBool,
}

pub struct TestServer {
    pub url: String,
    pub state: Arc<ServerState>,
}

impl TestServer {
    pub fn start(library: Library, faults: Faults) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(ServerState { library: Mutex::new(library), faults: Mutex::new(faults), requests: Mutex::new(Vec::new()), extension: Mutex::new(None), decisions: Mutex::new(Vec::new()), failures: Mutex::new(0), down: AtomicBool::new(false), seen: Default::default(), form_post: AtomicBool::new(false) });
        let st = state.clone();
        thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                st.seen.fetch_add(1, Ordering::AcqRel);
                if st.down.load(Ordering::Acquire) {
                    continue;
                }
                let st = st.clone();
                thread::spawn(move || {
                    let _ = handle(conn, st);
                });
            }
        });
        Self { url, state }
    }

    pub fn seen(&self) -> usize {
        self.state.seen.load(Ordering::Acquire)
    }

    /// Simulates the server going away, and coming back.
    pub fn set_down(&self, down: bool) {
        self.state.down.store(down, Ordering::Release);
    }

    pub fn requests(&self) -> Vec<Req> {
        self.state.requests.lock().unwrap().clone()
    }
}

fn param<'a>(query: &'a str, key: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes()).find(|(k, _)| k == key).map(|(_, v)| v.into_owned())
}

fn handle(mut conn: TcpStream, st: Arc<ServerState>) -> std::io::Result<()> {
    conn.set_nodelay(true).ok();
    loop {
        let mut reader = BufReader::new(conn.try_clone()?);
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || st.down.load(Ordering::Acquire) {
            return Ok(());
        }
        st.seen.fetch_add(1, Ordering::AcqRel);
        let method = line.split_whitespace().next().unwrap_or("GET").to_string();
        let target = line.split_whitespace().nth(1).unwrap_or("/").to_string();
        let mut range = None;
        let mut content_length = 0usize;
        let mut head = String::new();
        loop {
            head.clear();
            if reader.read_line(&mut head)? == 0 || head == "\r\n" {
                break;
            }
            if let Some(v) = head.to_ascii_lowercase().strip_prefix("range: ") {
                range = Some(v.trim().to_string());
            }
            if let Some(v) = head.to_ascii_lowercase().strip_prefix("content-length: ") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
        let path = target.split('?').next().unwrap_or("");
        let endpoint = path.rsplit('/').next().unwrap_or("").to_string();
        let url_query = target.split_once('?').map(|(_, q)| q).unwrap_or("").to_string();
        if endpoint == "getOpenSubsonicExtensions" {
            let mut names = Vec::new();
            if st.extension.lock().unwrap().is_some() {
                names.push(r#"{"name":"transcoding","versions":[1]}"#);
            }
            if st.form_post.load(Ordering::Acquire) {
                names.push(r#"{"name":"formPost","versions":[1]}"#);
            }
            let body = format!(r#"{{"subsonic-response":{{"status":"ok","openSubsonicExtensions":[{}]}}}}"#, names.join(","));
            write!(conn, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len())?;
            continue;
        }
        if endpoint == "getTranscodeDecision" {
            let mut raw = vec![0u8; content_length];
            std::io::Read::read_exact(&mut reader, &mut raw)?;
            let request = String::from_utf8_lossy(&raw).to_string();
            let format = request.split(r#""container":""#).nth(1).and_then(|s| s.split('"').next()).unwrap_or("mp3").to_string();
            st.decisions.lock().unwrap().push(request);
            let fake = st.extension.lock().unwrap().clone().unwrap_or_default();
            let body = format!(r#"{{"subsonic-response":{{"status":"ok","transcodeDecision":{{"canDirectPlay":{},"canTranscode":{},"transcodeParams":"{format}"}}}}}}"#, fake.can_direct_play, fake.can_transcode);
            write!(conn, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len())?;
            continue;
        }
        let mut form = String::new();
        if method == "POST" && content_length > 0 {
            let mut raw = vec![0u8; content_length];
            std::io::Read::read_exact(&mut reader, &mut raw)?;
            form = String::from_utf8_lossy(&raw).to_string();
        }
        let keys = |q: &str| url::form_urlencoded::parse(q.as_bytes()).map(|(k, _)| k.into_owned()).collect::<Vec<_>>();
        let (query_keys, body_keys) = (keys(&url_query), keys(&form));
        let joined = if form.is_empty() { url_query.clone() } else { format!("{url_query}&{form}") };
        let query = joined.as_str();
        let id = param(query, "id").or_else(|| param(query, "mediaId")).unwrap_or_default();
        // A decision's token is the fake's format.
        let format = param(query, "format").or_else(|| param(query, "transcodeParams"));
        let time_offset = param(query, "timeOffset").or_else(|| param(query, "offset")).and_then(|v| v.parse().ok());
        let max_bit_rate = param(query, "maxBitRate").and_then(|v| v.parse().ok());
        st.requests.lock().unwrap().push(Req { id: id.clone(), format: format.clone(), time_offset, max_bit_rate, range: range.clone(), endpoint, method, query_keys, body_keys });
        let faults = *st.faults.lock().unwrap();

        let transcode = format.as_deref().filter(|f| *f != "raw").map(|f| f.to_string());
        if let Some(fmt) = transcode {
            let lib = st.library.lock().unwrap();
            let Some(data) = lib.transcoded.get(&(id.clone(), fmt.clone())).cloned() else {
                drop(lib);
                not_found(&mut conn)?;
                continue;
            };
            drop(lib);
            let mime = faults.transcode_mime.unwrap_or(if fmt == "mp3" { "audio/mpeg" } else { "audio/ogg" });
            // Live transcodes have no length and no ranges.
            write!(conn, "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nAccept-Ranges: none\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")?;
            for chunk in data.chunks(16 * 1024) {
                if st.down.load(Ordering::Acquire) {
                    return Err(std::io::Error::other("server went down"));
                }
                write!(conn, "{:x}\r\n", chunk.len())?;
                conn.write_all(chunk)?;
                conn.write_all(b"\r\n")?;
                if faults.throttle > 0 {
                    thread::sleep(Duration::from_secs_f64(chunk.len() as f64 / faults.throttle as f64));
                }
            }
            conn.write_all(b"0\r\n\r\n")?;
            return Ok(());
        }

        let lib = st.library.lock().unwrap();
        let Some(data) = lib.raw.get(&id).cloned() else {
            drop(lib);
            not_found(&mut conn)?;
            continue;
        };
        let stamp = lib.stamps.get(&id).map(|s| format!("Last-Modified: {s}\r\n")).unwrap_or_default();
        drop(lib);

        if range.is_some() && !faults.no_ranges {
            {
                let mut f = st.failures.lock().unwrap();
                if *f < faults.fail_first {
                    *f += 1;
                    write!(conn, "HTTP/1.1 500 Oops\r\nContent-Length: 0\r\n\r\n")?;
                    continue;
                }
            }
            thread::sleep(faults.stall);
            let spec = range.unwrap();
            let spec = spec.trim_start_matches("bytes=");
            let (a, b) = spec.split_once('-').unwrap_or((spec, ""));
            let total = data.len() as u64;
            let start: u64 = a.parse().unwrap_or(0);
            let end: u64 = if b.is_empty() { total - 1 } else { b.parse::<u64>().unwrap().min(total - 1) };
            let body = &data[start as usize..=end as usize];
            write!(conn, "HTTP/1.1 206 Partial Content\r\nContent-Type: audio/flac\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{total}\r\n{stamp}Content-Length: {}\r\n\r\n", body.len())?;
            send_body(&mut conn, body, faults.throttle, &st)?;
        } else {
            write!(conn, "HTTP/1.1 200 OK\r\nContent-Type: audio/flac\r\n{stamp}Content-Length: {}\r\n\r\n", data.len())?;
            send_body(&mut conn, &data, faults.throttle, &st)?;
        }
        // Keep-alive loop so reqwest reuses the connection.
    }
}

fn send_body(conn: &mut TcpStream, body: &[u8], throttle: u64, st: &ServerState) -> std::io::Result<()> {
    if throttle == 0 && !st.down.load(Ordering::Acquire) {
        return conn.write_all(body);
    }
    for chunk in body.chunks(32 * 1024) {
        if st.down.load(Ordering::Acquire) {
            return Err(std::io::Error::other("server went down"));
        }
        conn.write_all(chunk)?;
        if throttle > 0 {
            thread::sleep(Duration::from_secs_f64(chunk.len() as f64 / throttle as f64));
        }
    }
    Ok(())
}

fn not_found(conn: &mut TcpStream) -> std::io::Result<()> {
    write!(conn, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
}

