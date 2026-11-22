//! whether Subsonic requests carry their parameters in a POST form (OpenSubsonic `formPost`), so
//! the credentials stay out of URLs and out of server and proxy logs.
//!
//! URLs are always built with every parameter in the query. a request Navia sends itself goes
//! through [`FormPost::request`] or [`FormPost::request_blocking`], which move that query into an
//! `application/x-www-form-urlencoded` body when the server takes it. a URL handed to anything
//! outside Navia keeps its query and is fetched with GET.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::header::CONTENT_TYPE;
use serde_json::Value;

use super::{API_VERSION, CLIENT_NAME, OfflineSwitch};

const UNKNOWN: u8 = 0;
const GET: u8 = 1;
const POST: u8 = 2;

const ASK_TIMEOUT: Duration = Duration::from_secs(5);
/// after an unanswered ask, requests go out as GET for this long before asking again.
const ASK_AGAIN_AFTER: Duration = Duration::from_secs(30);

const FORM: &str = "application/x-www-form-urlencoded";

/// shared by a client, its copies, and the streams and downloads made from it.
#[derive(Clone)]
pub struct FormPost(Arc<Inner>);

struct Inner {
    /// `http://host/rest/`: only URLs under it are the server's Subsonic API.
    rest_base: String,
    state: AtomicU8,
    unanswered_at: Mutex<Option<Instant>>,
    asking: tokio::sync::Mutex<()>,
    asking_blocking: Mutex<()>,
    offline: OfflineSwitch,
}

/// what an extension list said, or that it said nothing usable.
enum Answer {
    Known(bool),
    Unanswered,
}

impl FormPost {
    pub(crate) fn new(rest_base: String, offline: OfflineSwitch) -> Self {
        Self(Arc::new(Inner { rest_base, state: AtomicU8::new(UNKNOWN), unanswered_at: Mutex::new(None), asking: Default::default(), asking_blocking: Mutex::new(()), offline }))
    }

    /// `None` until the server's extension list was read.
    pub fn known(&self) -> Option<bool> {
        match self.0.state.load(Ordering::Acquire) {
            GET => Some(false),
            POST => Some(true),
            _ => None,
        }
    }

    pub(crate) fn set(&self, on: bool) {
        self.0.state.store(if on { POST } else { GET }, Ordering::Release);
    }

    /// the URL without its query and the query as a form body, when this URL should go as a POST.
    fn split<'a>(&self, url: &'a str) -> Option<(&'a str, &'a str)> {
        if self.0.state.load(Ordering::Acquire) != POST || !url.starts_with(&self.0.rest_base) {
            return None;
        }
        url.split_once('?')
    }

    /// a request for `url` with the async client: a POST form when the server takes one.
    pub async fn request(&self, http: &reqwest::Client, url: &str) -> reqwest::RequestBuilder {
        self.learn(http).await;
        match self.split(url) {
            Some((path, body)) => http.post(path).header(CONTENT_TYPE, FORM).body(body.to_string()),
            None => http.get(url),
        }
    }

    /// [`Self::request`] for a blocking client. never call it on an async runtime's thread.
    pub fn request_blocking(&self, http: &reqwest::blocking::Client, url: &str) -> reqwest::blocking::RequestBuilder {
        self.learn_blocking(http);
        match self.split(url) {
            Some((path, body)) => http.post(path).header(CONTENT_TYPE, FORM).body(body.to_string()),
            None => http.get(url),
        }
    }

    /// a POST of `url`'s parameters as a form when the server takes one. otherwise the URL is sent
    /// as is and `fallback` is the form body, as servers without `formPost` still read a POST form.
    pub(crate) async fn post_form(&self, http: &reqwest::Client, url: &str, fallback: (&str, String)) -> reqwest::RequestBuilder {
        self.learn(http).await;
        match self.split(url) {
            Some((path, body)) => http.post(path).header(CONTENT_TYPE, FORM).body(body.to_string()),
            None => http.post(fallback.0).header(CONTENT_TYPE, FORM).body(fallback.1),
        }
    }

    fn should_ask(&self) -> bool {
        self.0.state.load(Ordering::Acquire) == UNKNOWN && !self.0.offline.is_on() && self.0.unanswered_at.lock().expect("form post").is_none_or(|at| at.elapsed() >= ASK_AGAIN_AFTER)
    }

    /// the extension list needs no sign-in, so asking for it sends no credentials.
    fn ask_url(&self) -> String {
        format!("{}getOpenSubsonicExtensions?v={API_VERSION}&c={CLIENT_NAME}&f=json", self.0.rest_base)
    }

    async fn learn(&self, http: &reqwest::Client) {
        if !self.should_ask() {
            return;
        }
        let _asking = self.0.asking.lock().await;
        if !self.should_ask() {
            return;
        }
        let answer = match http.get(self.ask_url()).timeout(ASK_TIMEOUT).send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                read_answer(status, &response.bytes().await.unwrap_or_default())
            }
            Err(_) => Answer::Unanswered,
        };
        self.note(answer);
    }

    fn learn_blocking(&self, http: &reqwest::blocking::Client) {
        if !self.should_ask() {
            return;
        }
        let _asking = self.0.asking_blocking.lock().expect("form post");
        if !self.should_ask() {
            return;
        }
        let answer = match http.get(self.ask_url()).timeout(ASK_TIMEOUT).send() {
            Ok(response) => {
                let status = response.status().as_u16();
                read_answer(status, &response.bytes().unwrap_or_default())
            }
            Err(_) => Answer::Unanswered,
        };
        self.note(answer);
    }

    fn note(&self, answer: Answer) {
        match answer {
            Answer::Known(on) => {
                // a signed-in read of the list may have answered meanwhile, and it wins
                let _ = self.0.state.compare_exchange(UNKNOWN, if on { POST } else { GET }, Ordering::AcqRel, Ordering::Acquire);
            }
            Answer::Unanswered => *self.0.unanswered_at.lock().expect("form post") = Some(Instant::now()),
        }
    }
}

/// a server that answers without the extension, or with an error, is taken at its word until a
/// signed-in read of the list says otherwise; a failure of the server itself decides nothing.
fn read_answer(status: u16, body: &[u8]) -> Answer {
    if status >= 500 {
        return Answer::Unanswered;
    }
    let response = serde_json::from_slice::<Value>(body).ok().and_then(|v| v.get("subsonic-response").cloned());
    let Some(response) = response else {
        return if (400..500).contains(&status) { Answer::Known(false) } else { Answer::Unanswered };
    };
    let listed = response.get("openSubsonicExtensions").and_then(Value::as_array).is_some_and(|list| list.iter().any(|e| e.get("name").and_then(Value::as_str) == Some("formPost")));
    Answer::Known(listed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(state: u8) -> FormPost {
        let f = FormPost::new("http://h/rest/".to_string(), OfflineSwitch::default());
        f.0.state.store(state, Ordering::Release);
        f
    }

    #[test]
    fn only_the_servers_own_api_moves_into_a_form() {
        let on = form(POST);
        assert_eq!(on.split("http://h/rest/stream?u=a&t=b&s=c&id=1"), Some(("http://h/rest/stream", "u=a&t=b&s=c&id=1")));
        assert_eq!(on.split("http://radio.example/rest/stream?id=1"), None);
        assert_eq!(on.split("http://h/share/img/x?size=600"), None);
        assert_eq!(form(GET).split("http://h/rest/stream?u=a"), None);
        assert_eq!(form(UNKNOWN).split("http://h/rest/stream?u=a"), None);
    }

    #[test]
    fn answers_decide_only_when_the_server_itself_answered() {
        let listed = br#"{"subsonic-response":{"status":"ok","openSubsonicExtensions":[{"name":"formPost","versions":[1]}]}}"#;
        let unlisted = br#"{"subsonic-response":{"status":"ok","openSubsonicExtensions":[]}}"#;
        let refused = br#"{"subsonic-response":{"status":"failed","error":{"code":40}}}"#;
        assert!(matches!(read_answer(200, listed), Answer::Known(true)));
        assert!(matches!(read_answer(200, unlisted), Answer::Known(false)));
        assert!(matches!(read_answer(200, refused), Answer::Known(false)));
        assert!(matches!(read_answer(404, b"not found"), Answer::Known(false)));
        assert!(matches!(read_answer(502, b""), Answer::Unanswered));
        assert!(matches!(read_answer(200, b"<html>"), Answer::Unanswered));
    }

    #[test]
    fn a_signed_in_answer_is_not_overwritten() {
        let f = form(UNKNOWN);
        f.set(true);
        f.note(Answer::Known(false));
        assert_eq!(f.known(), Some(true));
    }
}
