//! The receiver's remote control (DACP). The receiver finds us as `iTunes_Ctrl_<DACP-ID>` over mDNS
//! and sends `GET /ctrl-int/1/<command>` with our `Active-Remote` token. Other tokens are refused.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::discovery::{Announcement, Discovery};
use crate::error::{Error, Result};

/// A press on the receiver's remote.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RemoteCommand {
    PlayPause,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    VolumeUp,
    VolumeDown,
}

pub type RemoteHandler = Arc<dyn Fn(RemoteCommand) + Send + Sync>;

/// What a DACP request asks for. A volume goes to the session, never to the app or back to the receiver.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Request {
    Remote(RemoteCommand),
    /// 0 to 1, Navia's scale.
    Volume(f32),
}

pub(crate) type RequestHandler = Arc<dyn Fn(Request) + Send + Sync>;

/// The ids a session puts in its RTSP headers so the receiver's remote can find us.
#[derive(Clone, Debug)]
pub struct RemoteIds {
    /// 16 hex digits.
    pub dacp_id: String,
    pub active_remote: String,
}

impl RemoteIds {
    pub fn new() -> Self {
        Self { dacp_id: format!("{:016X}", rand::random::<u64>()), active_remote: format!("{}", rand::random::<u32>()) }
    }
}

pub struct Dacp {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    announcement: Option<Announcement>,
}

impl Dacp {
    /// listen on `port` (0 for any) and announce it through `discovery`.
    pub fn start(discovery: &Discovery, port: u16, ids: &RemoteIds, handler: RequestHandler) -> Result<Self> {
        let listener = match TcpListener::bind(("0.0.0.0", port)) {
            Ok(l) => l,
            Err(e) if port != 0 => {
                tracing::warn!("airplay: remote-control port {port} is taken ({e}), using any port");
                TcpListener::bind(("0.0.0.0", 0))?
            }
            Err(e) => return Err(e.into()),
        };
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let txt = [("txtvers", "1"), ("Ver", "131075"), ("DbId", ids.dacp_id.as_str()), ("OSsi", "0x1F5")];
        let host = format!("navia-{}", ids.dacp_id.to_lowercase());
        let announcement = discovery.announce(&format!("iTunes_Ctrl_{}", ids.dacp_id), "_dacp._tcp", &host, port, &txt)?;
        let stop = Arc::new(AtomicBool::new(false));
        let (flag, token) = (stop.clone(), ids.active_remote.clone());
        let thread = thread::Builder::new()
            .name("navia-airplay-dacp".into())
            .spawn(move || {
                while !flag.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => serve(stream, &token, &handler),
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(100)),
                        Err(_) => thread::sleep(Duration::from_millis(100)),
                    }
                }
            })
            .map_err(Error::Io)?;
        Ok(Self { stop, thread: Some(thread), announcement: Some(announcement) })
    }
}

impl Drop for Dacp {
    fn drop(&mut self) {
        self.announcement.take();
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Serves one request. Receivers send one per connection, or a few with keep-alive.
fn serve(stream: TcpStream, token: &str, handler: &RequestHandler) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let Ok(read) = stream.try_clone() else { return };
    let mut reader = BufReader::new(read);
    let mut out = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
        let mut remote = None;
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':')
                && k.trim().eq_ignore_ascii_case("Active-Remote")
            {
                remote = Some(v.trim().to_string());
            }
        }
        let ok = remote.as_deref() == Some(token);
        let status = if !ok {
            "403 Forbidden"
        } else if let Some(cmd) = parse(&path) {
            handler(cmd);
            "204 No Content"
        } else {
            "400 Bad Request"
        };
        if write!(out, "HTTP/1.1 {status}\r\nDAAP-Server: iTunes/11.0.5 (OS X)\r\nContent-Length: 0\r\n\r\n").is_err() {
            return;
        }
    }
}

/// `/ctrl-int/1/playpause`, `.../setproperty?dmcp.device-volume=-15.0`, ...
fn parse(path: &str) -> Option<Request> {
    let rest = path.strip_prefix("/ctrl-int/1/")?;
    let (cmd, query) = rest.split_once('?').unwrap_or((rest, ""));
    Some(Request::Remote(match cmd {
        "playpause" => RemoteCommand::PlayPause,
        "play" | "playresume" => RemoteCommand::Play,
        "pause" => RemoteCommand::Pause,
        "stop" => RemoteCommand::Stop,
        "nextitem" => RemoteCommand::Next,
        "previtem" => RemoteCommand::Previous,
        "volumeup" => RemoteCommand::VolumeUp,
        "volumedown" => RemoteCommand::VolumeDown,
        "setproperty" => {
            let db = query.split('&').find_map(|kv| kv.strip_prefix("dmcp.device-volume="))?.parse::<f32>().ok().filter(|v| v.is_finite())?;
            return Some(Request::Volume(super::session::db_to_level(db)));
        }
        _ => return None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands() {
        assert_eq!(parse("/ctrl-int/1/playpause"), Some(Request::Remote(RemoteCommand::PlayPause)));
        assert_eq!(parse("/ctrl-int/1/nextitem"), Some(Request::Remote(RemoteCommand::Next)));
        assert_eq!(parse("/ctrl-int/1/setproperty?dmcp.device-volume=-15.000000"), Some(Request::Volume(0.5)));
        assert_eq!(parse("/ctrl-int/1/setproperty?dmcp.device-volume=-144.0"), Some(Request::Volume(0.0)));
        assert_eq!(parse("/ctrl-int/1/setproperty?dmcp.device-volume=nan"), None);
        assert_eq!(parse("/ctrl-int/1/beginff"), None);
        assert_eq!(parse("/other"), None);
    }

    #[test]
    fn only_the_token_holder_is_obeyed() {
        let got = Arc::new(std::sync::Mutex::new(Vec::new()));
        let g = got.clone();
        let handler: RequestHandler = Arc::new(move |c| g.lock().unwrap().push(c));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let t = thread::spawn(move || {
            for _ in 0..2 {
                let (s, _) = listener.accept().unwrap();
                serve(s, "1234", &handler);
            }
        });
        for token in ["999", "1234"] {
            let mut c = TcpStream::connect(addr).unwrap();
            write!(c, "GET /ctrl-int/1/pause HTTP/1.1\r\nActive-Remote: {token}\r\n\r\n").unwrap();
            let mut first = String::new();
            BufReader::new(&c).read_line(&mut first).unwrap();
            assert!(first.contains(if token == "1234" { "204" } else { "403" }), "{first}");
        }
        t.join().unwrap();
        assert_eq!(*got.lock().unwrap(), vec![Request::Remote(RemoteCommand::Pause)]);
    }
}
