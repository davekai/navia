//! Streams one track over HTTP into an anonymous temp file and reads it back as a [`MediaSource`].
//! Sources with byte ranges are fetched in bounded chunks around the read position, so seeks are
//! cheap. Sources without ranges, such as live transcodes, download start to finish.
//!
//! A [`CompleteSink`] can keep a stream once every byte is on disk. Holes left by seeking are only
//! filled while the track is loaded, and a skipped track's partial bytes are dropped.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use reqwest::header::{CONTENT_RANGE, CONTENT_TYPE, LAST_MODIFIED, RANGE};
use symphonia::core::io::MediaSource;

use crate::error::{Error, Result};
use crate::songcache::Spool;

/// Takes a stream's bytes once all of them are on disk.
pub trait CompleteSink: Send + Sync {
    /// Whether holes behind the reader may be fetched so the file completes.
    /// Asked again each time the stream has nothing else to do.
    fn fill_gaps(&self) -> bool {
        false
    }

    /// Called once on the stream's thread, with bytes 0 to `len` readable from `spool`.
    /// `last_modified` is the server's header, if it sent one.
    fn complete(&self, spool: &dyn Spool, len: u64, last_modified: Option<&str>);
}

#[derive(Default)]
struct SinkSlot {
    sink: Option<Arc<dyn CompleteSink>>,
    /// The length, when the stream finished before a sink was attached.
    completed: Option<u64>,
}

/// Tuning for one stream. The defaults are sized for typical FLAC.
#[derive(Clone, Copy, Debug)]
pub struct StreamConfig {
    /// First chunk's size. A probe needs only the head of the file.
    pub initial_chunk: u64,
    pub max_chunk: u64,
    /// Bytes to keep buffered ahead of the read position.
    pub read_ahead: u64,
    /// Failed fetches in a row before giving up on this track's own errors.
    /// Losing the server never gives up, it waits and resumes.
    pub max_retries: u32,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self { initial_chunk: 256 * 1024, max_chunk: 4 * 1024 * 1024, read_ahead: 12 * 1024 * 1024, max_retries: 4 }
    }
}

/// How long a stream waits between tries once the server is unreachable. Doubles up to the max.
pub const RETRY_MIN: Duration = Duration::from_millis(500);
pub const RETRY_MAX: Duration = Duration::from_secs(15);
/// Failures in a row before a stream counts as waiting for the server, not just a blip.
const WAITING_AFTER: u32 = 2;

/// Whether any of a player's streams is waiting for the server, and a way to hurry them along.
#[derive(Default)]
pub struct NetworkStatus {
    waiting: AtomicUsize,
    retry_generation: AtomicU64,
    message: Mutex<String>,
}

impl NetworkStatus {
    pub fn waiting(&self) -> bool {
        self.waiting.load(Ordering::Acquire) > 0
    }

    /// The last failure, for messages. Never holds a URL.
    pub fn message(&self) -> String {
        self.message.lock().expect("network status").clone()
    }

    /// Asks every waiting stream to try again now instead of finishing its backoff.
    pub fn retry_now(&self) {
        self.retry_generation.fetch_add(1, Ordering::AcqRel);
    }

    fn generation(&self) -> u64 {
        self.retry_generation.load(Ordering::Acquire)
    }

    fn set_message(&self, message: String) {
        *self.message.lock().expect("network status") = message;
    }
}

/// HTTP clients shared by all streams. Build one per player.
#[derive(Clone)]
pub struct HttpFetcher {
    ranged: Client,
    sequential: Client,
    status: Arc<NetworkStatus>,
    offline: crate::client::OfflineSwitch,
    form: Option<crate::client::FormPost>,
}

impl HttpFetcher {
    pub fn new() -> Result<Self> {
        let ua = crate::client::user_agent();
        let ranged = Client::builder().user_agent(ua.clone()).connect_timeout(Duration::from_secs(10)).timeout(Duration::from_secs(60)).build()?;
        // Live transcodes can legitimately take the whole track's length.
        let sequential = Client::builder().user_agent(ua).connect_timeout(Duration::from_secs(10)).timeout(None::<Duration>).build()?;
        Ok(Self { ranged, sequential, status: Arc::default(), offline: Default::default(), form: None })
    }

    /// streams hold their URL, so a switch turned on mid-song stops the next request too.
    pub fn with_offline(mut self, offline: crate::client::OfflineSwitch) -> Self {
        self.offline = offline;
        self
    }

    /// the server's own URLs go as POST forms when it takes them.
    pub fn with_form_post(mut self, form: crate::client::FormPost) -> Self {
        self.form = Some(form);
        self
    }

    fn get(&self, http: &Client, url: &str) -> reqwest::blocking::RequestBuilder {
        match &self.form {
            Some(form) => form.request_blocking(http, url),
            None => http.get(url),
        }
    }

    pub fn status(&self) -> Arc<NetworkStatus> {
        self.status.clone()
    }
}

/// Byte intervals already on disk, kept coalesced.
#[derive(Default)]
struct Ranges(BTreeMap<u64, u64>);

impl Ranges {
    fn insert(&mut self, mut start: u64, mut end: u64) {
        if let Some((&s, &e)) = self.0.range(..=start).next_back()
            && e >= start
        {
            start = s;
            end = end.max(e);
            self.0.remove(&s);
        }
        let swallowed: Vec<u64> = self.0.range(start..=end).map(|(&s, _)| s).collect();
        for s in swallowed {
            let e = self.0.remove(&s).expect("present");
            end = end.max(e);
        }
        self.0.insert(start, end);
    }

    /// Bytes available contiguously from `pos`.
    fn available_from(&self, pos: u64) -> u64 {
        match self.0.range(..=pos).next_back() {
            Some((_, &e)) if e > pos => e - pos,
            _ => 0,
        }
    }

    /// The first byte at or after `pos` that is missing, and the end of that gap.
    fn first_gap(&self, pos: u64) -> (u64, Option<u64>) {
        let mut at = pos;
        if let Some((_, &e)) = self.0.range(..=pos).next_back()
            && e > at
        {
            at = e;
        }
        let end = self.0.range(at + 1..).next().map(|(&s, _)| s);
        (at, end)
    }
}

struct State {
    buffered: Ranges,
    /// Reader cursor the worker is serving.
    want: u64,
    /// True once a reader has asked for something, so the worker may run ahead.
    started: bool,
    eof: Option<u64>,
    error: Option<String>,
    bytes: u64,
    requests: u64,
    cancelled: bool,
    /// Set by [`HttpStream::interrupt`]. A reader blocked on missing bytes gives up its read.
    interrupted: bool,
    /// True when `error` is the server going away, not this track's own problem.
    lost: bool,
}

struct Shared {
    state: Mutex<State>,
    cond: Condvar,
    file: Mutex<File>,
    total: Option<u64>,
    ranged: bool,
    config: StreamConfig,
    cancelled: AtomicBool,
    /// While a track opens, losing the server fails the open and the engine retries it,
    /// since a reader stuck on a dead probe couldn't be interrupted.
    opening: AtomicBool,
    content_type: String,
    last_modified: Option<String>,
    /// What to do with the bytes once they're all here.
    sink: Mutex<SinkSlot>,
}

/// Measurement of one stream's network use.
#[derive(Clone, Copy, Debug, Default)]
pub struct StreamStats {
    pub requests: u64,
    pub bytes: u64,
}

/// One track being streamed. Dropping it stops the download.
pub struct HttpStream {
    shared: Arc<Shared>,
}

impl fmt::Debug for HttpStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpStream").field("total", &self.shared.total).field("ranged", &self.shared.ranged).finish()
    }
}

/// One independent read position over a stream.
pub struct HttpReader {
    // Holding the stream keeps the download alive until the last reader is gone.
    stream: Arc<HttpStream>,
    cursor: u64,
}

fn io_err(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

impl HttpStream {
    /// Connects and starts downloading. Fails on a bad status or a Subsonic error document, so
    /// callers can fall back before any audio plays.
    pub fn open(fetcher: &HttpFetcher, url: String, config: StreamConfig) -> Result<Arc<Self>> {
        let file = tempfile::tempfile()?;
        let resp = open_with_retries(fetcher, &url, &config)?;
        let status = resp.status();
        let content_type = header(&resp, CONTENT_TYPE).unwrap_or_default();
        let content_type_kept = content_type.clone();
        let last_modified = header(&resp, LAST_MODIFIED);
        if content_type.contains("json") || content_type.contains("xml") {
            return Err(subsonic_error(resp));
        }
        let (total, ranged) = if status == StatusCode::PARTIAL_CONTENT {
            // `Content-Range: bytes 0-99/*` is legal, but chunked fetching needs the length.
            let Some(total) = parse_total(&header(&resp, CONTENT_RANGE).unwrap_or_default()) else {
                return Err(Error::Audio("the server sent a partial response with no length".to_string()));
            };
            (Some(total), true)
        } else {
            (None, false)
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                buffered: Ranges::default(),
                want: 0,
                started: false,
                eof: if ranged { total } else { None },
                error: None,
                bytes: 0,
                requests: 1,
                cancelled: false,
                interrupted: false,
                lost: false,
            }),
            cond: Condvar::new(),
            file: Mutex::new(file),
            total,
            ranged,
            config,
            cancelled: AtomicBool::new(false),
            opening: AtomicBool::new(true),
            content_type: content_type_kept,
            last_modified,
            sink: Mutex::new(SinkSlot::default()),
        });
        let (worker_shared, fetcher, url_owned) = (shared.clone(), fetcher.clone(), url);
        thread::Builder::new()
            .name("navia-http".into())
            .spawn(move || {
                let _guard = FailOnPanic(worker_shared.clone());
                worker(worker_shared, fetcher, url_owned, resp)
            })
            .map_err(Error::Io)?;
        Ok(Arc::new(Self { shared }))
    }

    pub fn reader(self: &Arc<Self>) -> HttpReader {
        HttpReader { stream: self.clone(), cursor: 0 }
    }

    /// True when the server honored byte ranges.
    pub fn is_seekable(&self) -> bool {
        self.shared.ranged && self.shared.total.is_some()
    }

    /// Marks the track as opened. From here on, losing the server waits inside the read instead of failing.
    pub(crate) fn finish_opening(&self) {
        self.shared.opening.store(false, Ordering::Release);
    }

    /// True if the server went away while this stream was reading.
    pub(crate) fn lost(&self) -> bool {
        self.shared.state.lock().expect("stream state").lost
    }

    /// Makes a read blocked on missing bytes return, so the engine can act on a command.
    /// Reads that have data are unaffected, so this is safe to call at any time.
    pub fn interrupt(&self) {
        self.shared.state.lock().expect("stream state").interrupted = true;
        self.shared.cond.notify_all();
    }

    /// The response's content type.
    pub fn content_type(&self) -> &str {
        &self.shared.content_type
    }

    /// Hands the bytes to `sink` once all of them are on disk.
    /// If they already are, it runs now on its own thread.
    pub fn keep_when_complete(&self, sink: Arc<dyn CompleteSink>) {
        let mut slot = self.shared.sink.lock().expect("stream sink");
        if let Some(len) = slot.completed.take() {
            drop(slot);
            let shared = self.shared.clone();
            let _ = thread::Builder::new().name("navia-keep".into()).spawn(move || sink.complete(&SpoolOf(&shared), len, shared.last_modified.as_deref()));
            return;
        }
        slot.sink = Some(sink);
        drop(slot);
        // A worker idling with holes behind the reader may now fill them.
        self.shared.cond.notify_all();
    }

    /// The reader survived an interrupt and is staying on this stream.
    pub(crate) fn clear_interrupt(&self) {
        self.shared.state.lock().expect("stream state").interrupted = false;
    }

    pub fn stats(&self) -> StreamStats {
        let st = self.shared.state.lock().expect("stream state");
        StreamStats { requests: st.requests, bytes: st.bytes }
    }
}

impl Drop for HttpStream {
    fn drop(&mut self) {
        self.shared.cancelled.store(true, Ordering::Release);
        self.shared.state.lock().expect("stream state").cancelled = true;
        self.shared.cond.notify_all();
    }
}

/// The first request, retrying transient failures so a blip doesn't skip the track.
fn open_with_retries(fetcher: &HttpFetcher, url: &str, config: &StreamConfig) -> Result<Response> {
    let mut failures = 0u32;
    loop {
        fetcher.offline.check()?;
        let result = fetcher.get(&fetcher.sequential, url).header(RANGE, format!("bytes=0-{}", config.initial_chunk - 1)).send().map_err(net_err).and_then(|resp| {
            let status = resp.status();
            if status == StatusCode::OK || status == StatusCode::PARTIAL_CONTENT {
                Ok(resp)
            } else {
                Err(Error::HttpStatus { status: status.as_u16(), endpoint: "stream".to_string() })
            }
        });
        match result {
            Ok(resp) => return Ok(resp),
        // Waiting out a lost server is the engine's job, so it can be interrupted.
            Err(e) if e.is_transient() && !e.is_outage() && failures < config.max_retries => {
                failures += 1;
                thread::sleep(Duration::from_millis(250 * u64::from(1u32 << failures.min(4))));
            }
            Err(e) => return Err(e),
        }
    }
}

fn net_err(e: reqwest::Error) -> Error {
    Error::Network(e.without_url().to_string())
}

fn header(resp: &Response, name: reqwest::header::HeaderName) -> Option<String> {
    resp.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
}

fn parse_total(content_range: &str) -> Option<u64> {
    content_range.rsplit('/').next()?.trim().parse().ok()
}

/// A 200 answer that holds a JSON or XML Subsonic error instead of audio.
fn subsonic_error(resp: Response) -> Error {
    let body = resp.text().unwrap_or_default();
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("subsonic-response")?.get("error")?.get("message")?.as_str().map(str::to_string))
        .unwrap_or_else(|| "the server sent a document instead of audio".to_string());
    let code = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("subsonic-response")?.get("error")?.get("code")?.as_i64())
        .unwrap_or(0) as i32;
    Error::Subsonic { code, endpoint: "stream".to_string(), message }
}

impl Shared {
    fn store(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        {
            let mut file = self.file.lock().expect("stream file");
            file.seek(SeekFrom::Start(offset))?;
            file.write_all(data)?;
        }
        let mut st = self.state.lock().expect("stream state");
        st.buffered.insert(offset, offset + data.len() as u64);
        st.bytes += data.len() as u64;
        drop(st);
        self.cond.notify_all();
        Ok(())
    }

    /// Every byte from 0 to `len` is on disk. Gives them to the sink, or notes it for a later one.
    fn complete(&self, len: u64) {
        let sink = {
            let mut slot = self.sink.lock().expect("stream sink");
            match slot.sink.take() {
                Some(sink) => sink,
                None => {
                    slot.completed = Some(len);
                    return;
                }
            }
        };
        sink.complete(&SpoolOf(self), len, self.last_modified.as_deref());
    }

    fn fill_gaps(&self) -> bool {
        self.sink.lock().expect("stream sink").sink.as_ref().is_some_and(|s| s.fill_gaps())
    }

    fn fail(&self, message: String, lost: bool) {
        let mut st = self.state.lock().expect("stream state");
        st.error = Some(message);
        st.lost = lost;
        drop(st);
        self.cond.notify_all();
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Reads the spool file at any offset.
struct SpoolOf<'a>(&'a Shared);

impl Spool for SpoolOf<'_> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let mut file = self.0.file.lock().expect("stream file");
        file.seek(SeekFrom::Start(offset))?;
        file.read(buf)
    }
}

/// What a response body is being read for.
#[derive(Clone, Copy)]
enum Chunk {
    /// A whole no-range response.
    Whole,
    /// Bytes the reader needs soon, from `start` to `end`.
    Ahead(u64, u64),
    /// A hole behind the reader, for completing the file. Gives way as soon as the reader waits.
    Fill,
}

/// Reads a response body into the file from `offset`. Returns false if it stopped early because
/// the stream was canceled or the reader no longer wants this range.
fn drain(shared: &Shared, mut resp: Response, offset: &mut u64, chunk: Chunk) -> io::Result<bool> {
    let mut buf = [0u8; 32 * 1024];
    loop {
        if shared.cancelled() {
            return Ok(false);
        }
        match chunk {
            Chunk::Whole => {}
            Chunk::Ahead(start, end) => {
                let st = shared.state.lock().expect("stream state");
                let wanted_elsewhere = st.started && (st.want > end || (st.want < start && st.buffered.available_from(st.want) == 0));
                if wanted_elsewhere {
                    return Ok(false);
                }
            }
            Chunk::Fill => {
                let st = shared.state.lock().expect("stream state");
                let starving = st.started && st.eof.is_none_or(|eof| st.want < eof) && st.buffered.available_from(st.want) == 0;
                if starving {
                    return Ok(false);
                }
            }
        }
        // Marked so the caller can tell a dropped connection from a local write failure.
        let n = resp.read(&mut buf).map_err(|e| io::Error::new(io::ErrorKind::ConnectionReset, e))?;
        if n == 0 {
            return Ok(true);
        }
        shared.store(*offset, &buf[..n])?;
        *offset += n as u64;
    }
}

/// If the worker thread panics, readers learn of it instead of blocking forever.
struct FailOnPanic(Arc<Shared>);

impl Drop for FailOnPanic {
    fn drop(&mut self) {
        if !thread::panicking() {
            return;
        }
        if let Ok(mut st) = self.0.state.lock() {
            st.error = Some("the stream stopped unexpectedly".to_string());
            drop(st);
            self.0.cond.notify_all();
        }
    }
}

enum Work {
    /// Fetch from `start` up to `end`. `fill` marks a hole behind the reader.
    Fetch { start: u64, end: u64, fill: bool },
    /// Every byte is on disk.
    Complete,
    Cancelled,
}

/// Decides what the worker should fetch next, or waits.
fn wait_for_work(shared: &Shared) -> Work {
    let total = shared.total.expect("ranged streams know their length");
    let mut st = shared.state.lock().expect("stream state");
    loop {
        // a reader that let go after the last byte still leaves a whole file to keep
        if st.buffered.first_gap(0).0 >= total {
            return Work::Complete;
        }
        if st.cancelled {
            return Work::Cancelled;
        }
        let (gap, gap_end) = st.buffered.first_gap(st.want);
        if gap < total && gap - st.want < shared.config.read_ahead {
            return Work::Fetch { start: gap, end: gap_end.unwrap_or(total).min(total), fill: false };
        }
        // The reader has what it needs. Stop when the whole file is here, or fill a hole behind
        // the reader when the sink wants the whole file.
        let (hole, hole_end) = st.buffered.first_gap(0);
        if hole < st.want && shared.fill_gaps() {
            return Work::Fetch { start: hole, end: hole_end.unwrap_or(total).min(total), fill: true };
        }
        st = shared.cond.wait(st).expect("stream state");
    }
}

/// Marks this stream as waiting for the server while it's out, however the worker exits.
struct Waiting<'a> {
    status: &'a NetworkStatus,
    active: bool,
}

impl Waiting<'_> {
    fn enter(&mut self) {
        if !self.active {
            self.active = true;
            self.status.waiting.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn leave(&mut self) {
        if self.active {
            self.active = false;
            self.status.waiting.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.leave();
    }
}

/// Sleeps for a backoff, stopping early if the stream is canceled or a retry is asked for.
fn backoff(shared: &Shared, status: &NetworkStatus, delay: Duration) {
    let generation = status.generation();
    let until = Instant::now() + delay;
    while Instant::now() < until && !shared.cancelled() && status.generation() == generation {
        thread::sleep(Duration::from_millis(50));
    }
}

fn worker(shared: Arc<Shared>, fetcher: HttpFetcher, url: String, first: Response) {
    if !shared.ranged {
        sequential(&shared, first);
        return;
    }
    let total = shared.total.expect("ranged has total");
    let cfg = shared.config;
    let status = fetcher.status.clone();
    let mut waiting = Waiting { status: &status, active: false };
    let mut failures = 0u32;
    let mut delay = RETRY_MIN;
    let mut chunk = cfg.initial_chunk;
    let mut first = Some(first);
    loop {
        // The first response, for the file's head, was already requested in `open`.
        let (start, end, fill, resp) = if let Some(resp) = first.take() {
            (0, cfg.initial_chunk.min(total), false, Ok(resp))
        } else {
            let (gap, gap_end, fill) = match wait_for_work(&shared) {
                Work::Fetch { start, end, fill } => (start, end, fill),
                Work::Complete => {
                    shared.complete(total);
                    return;
                }
                Work::Cancelled => return,
            };
            let end = gap_end.min(gap + chunk);
            let result = match fetcher.offline.check() {
                Ok(()) => fetcher.get(&fetcher.ranged, &url).header(RANGE, format!("bytes={gap}-{}", end - 1)).send().map_err(|e| e.without_url().to_string()),
                Err(e) => Err(e.to_string()),
            };
            shared.state.lock().expect("stream state").requests += 1;
            (gap, end, fill, result)
        };
        let started = Instant::now();
        let mut position = start;
        let mut outage = false;
        let kind = if fill { Chunk::Fill } else { Chunk::Ahead(start, end) };
        let outcome = match resp {
            Ok(r) if r.status() == StatusCode::PARTIAL_CONTENT => drain(&shared, r, &mut position, kind).inspect_err(|e| outage = e.kind() == io::ErrorKind::ConnectionReset),
            Ok(r) => {
                outage = matches!(r.status().as_u16(), 502..=504);
                Err(io_err(format!("server answered HTTP {} to a range request", r.status().as_u16())))
            }
            Err(e) => {
                outage = true;
                Err(io_err(e))
            }
        };
        match outcome {
            Ok(_) if shared.cancelled() => continue,
            Ok(finished) => {
                waiting.leave();
                delay = RETRY_MIN;
                if finished {
                    failures = 0;
                    // Aim for about two seconds of transfer so seeks stay responsive on slow links.
                    let elapsed = started.elapsed().as_secs_f64();
                    let moved = position - start;
                    chunk = if elapsed > 0.02 {
                        ((moved as f64 / elapsed * 2.0) as u64).clamp(cfg.initial_chunk, cfg.max_chunk)
                    } else {
                        (chunk * 2).min(cfg.max_chunk)
                    };
                }
            }
            Err(e) if outage => {
                // The server went away. Wait for it, keeping every byte already here.
                failures = failures.saturating_add(1);
                if failures >= WAITING_AFTER {
                    if shared.opening.load(Ordering::Acquire) {
                        shared.fail(format!("stream failed: {e}"), true);
                        return;
                    }
                    status.set_message(e.to_string());
                    waiting.enter();
                }
                backoff(&shared, &status, delay);
                delay = (delay * 2).min(RETRY_MAX);
                if shared.cancelled() {
                    return;
                }
            }
            Err(e) => {
                failures += 1;
                if failures > cfg.max_retries {
                    shared.fail(format!("stream failed: {e}"), false);
                    return;
                }
                thread::sleep(Duration::from_millis(250 * u64::from(1u32 << failures.min(4))));
            }
        }
    }
}

/// A live transcode: one response, read to its end.
fn sequential(shared: &Shared, resp: Response) {
    let mut offset = 0u64;
    // A body that says its length must arrive at that length to count as whole.
    let expected = resp.content_length();
    match drain(shared, resp, &mut offset, Chunk::Whole) {
        Ok(completed) => {
            let mut st = shared.state.lock().expect("stream state");
            st.eof = Some(offset);
            drop(st);
            shared.cond.notify_all();
            // a reader that let go after the last byte of a sized body still leaves a whole file
            let whole = match expected {
                Some(n) => offset == n,
                None => completed,
            };
            if whole {
                shared.complete(offset);
            }
        }
        Err(e) => shared.fail(format!("stream failed: {e}"), e.kind() == io::ErrorKind::ConnectionReset),
    }
}

impl Read for HttpReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut st = self.stream.shared.state.lock().expect("stream state");
        st.want = self.cursor;
        st.started = true;
        self.stream.shared.cond.notify_all();
        loop {
            let avail = st.buffered.available_from(self.cursor);
            if avail > 0 {
                drop(st);
                let n = (avail.min(buf.len() as u64)) as usize;
                let mut file = self.stream.shared.file.lock().expect("stream file");
                file.seek(SeekFrom::Start(self.cursor))?;
                file.read_exact(&mut buf[..n])?;
                self.cursor += n as u64;
                return Ok(n);
            }
            if let Some(eof) = st.eof
                && self.cursor >= eof
            {
                return Ok(0);
            }
            if let Some(err) = &st.error {
                let kind = if st.lost { io::ErrorKind::NotConnected } else { io::ErrorKind::Other };
                return Err(io::Error::new(kind, err.clone()));
            }
            if st.cancelled {
                return Err(io_err("stream cancelled"));
            }
            // A blocked read gives way to a skip, a seek or a stop.
            if st.interrupted {
                st.interrupted = false;
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "interrupted"));
            }
            let (next, _) = self.stream.shared.cond.wait_timeout(st, Duration::from_millis(200)).expect("stream state");
            st = next;
        }
    }
}

impl Seek for HttpReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let total = self.stream.shared.total;
        let new = match pos {
            SeekFrom::Start(p) => p,
            SeekFrom::Current(d) => self.cursor.saturating_add_signed(d),
            SeekFrom::End(d) => match total {
                Some(t) => t.saturating_add_signed(d),
                None => return Err(io_err("stream length unknown")),
            },
        };
        self.cursor = new;
        let mut st = self.stream.shared.state.lock().expect("stream state");
        st.want = new;
        st.started = true;
        drop(st);
        self.stream.shared.cond.notify_all();
        Ok(new)
    }
}

impl MediaSource for HttpReader {
    fn is_seekable(&self) -> bool {
        self.stream.shared.ranged && self.stream.shared.total.is_some()
    }

    fn byte_len(&self) -> Option<u64> {
        self.stream.shared.total
    }
}
