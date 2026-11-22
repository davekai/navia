//! Checks a server before signing in and says what went wrong, with one read-only `ping`.
//! The password only travels hashed into the token and never appears in error text.

use std::time::Duration;

use serde_json::Value;
use url::{Host, Url};

use crate::client::{Client, ClientConfig, Credentials};

/// How long a server gets to answer. Matches the text shown on the sign-in screen.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Set `NAVIA_DEV=1` to log in from `NAVIDROME_URL`, `NAVIDROME_USER` and `NAVIDROME_PASSWORD`.
/// It's never on by itself, so a stray `.env.local` can't override a user's own account.
pub const DEVELOPER_ENV: &str = "NAVIA_DEV";

pub fn developer_override() -> bool {
    std::env::var_os(DEVELOPER_ENV).is_some_and(|v| v == "1")
}

/// `(url, user, password)` from the environment, only under the developer override and with all three set.
pub fn developer_login() -> Option<(String, String, String)> {
    if !developer_override() {
        return None;
    }
    let get = |k| std::env::var(k).ok().filter(|v: &String| !v.is_empty());
    Some((get("NAVIDROME_URL")?, get("NAVIDROME_USER")?, get("NAVIDROME_PASSWORD")?))
}

/// A server address the user typed, made usable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address {
    /// `https://host:port/path`, with no trailing slash.
    pub url: String,
    pub host: String,
    /// Plain HTTP to something that isn't on the local network, so worth a warning.
    pub unencrypted: bool,
}

/// Turns what was typed into a server address. A missing scheme means https.
pub fn parse_address(raw: &str) -> Option<Address> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    let has_scheme = value.split("://").next().is_some_and(|s| value.contains("://") && !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')));
    let with_scheme = if has_scheme { value.to_string() } else { format!("https://{value}") };
    let mut url = Url::parse(&with_scheme).ok()?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    let host = url.host()?.to_owned();
    let host_text = url.host_str()?.to_string();
    // A bare word has no way to be a web address on its own, but `nas:4533` or `http://nas` can.
    if matches!(host, Host::Domain(ref d) if !d.contains('.') && d != "localhost") && !has_scheme && url.port().is_none() {
        return None;
    }
    let unencrypted = url.scheme() == "http" && !is_local_host(&host);
    url.set_query(None);
    url.set_fragment(None);
    Some(Address { url: url.as_str().trim_end_matches('/').to_string(), host: host_text, unencrypted })
}

fn is_local_host(host: &Host<String>) -> bool {
    match host {
        Host::Ipv4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1]),
        Host::Ipv6(ip) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00 || (ip.segments()[0] & 0xffc0) == 0xfe80,
        Host::Domain(d) => {
            let d = d.to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost") || d.ends_with(".local") || d.ends_with(".lan") || d.ends_with(".internal") || d.ends_with(".home.arpa")
        }
    }
}

/// What a server test found. Each variant maps to one message on the sign-in screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Probe {
    /// Navidrome answered and accepted this login.
    Connected { server: String, api: String, username: String },
    /// Navidrome answered, but no login was given to check.
    Found { server: String, api: String },
    /// The address can't be a server address at all.
    BadAddress,
    /// The connection was refused, from a wrong port or nothing listening.
    Refused,
    /// No answer within [`PROBE_TIMEOUT`].
    TimedOut,
    /// The name doesn't resolve.
    UnknownHost,
    /// The certificate is expired, untrusted or for another name.
    BadCertificate,
    /// A secure connection couldn't be made, usually https to a plain-http port.
    NotSecure,
    /// A connection failure that's none of the above.
    Unreachable { detail: String },
    /// A login was checked and refused (error 40).
    WrongLogin,
    /// The server doesn't accept token sign-in (error 41).
    TokenUnsupported,
    /// A Subsonic-style server that isn't Navidrome.
    NotNavidrome { name: String },
    /// Something answered, but not the Subsonic API.
    NotSubsonic,
    /// Navidrome answered with some other error.
    ServerError { code: i64, message: String },
}

impl Probe {
    /// True when signing in can go ahead.
    pub fn is_connected(&self) -> bool {
        matches!(self, Probe::Connected { .. })
    }
}

/// Checks `address` with this login. An empty username or password means "just check the server".
pub async fn probe(address: &str, username: &str, password: &str) -> Probe {
    probe_within(address, username, password, PROBE_TIMEOUT).await
}

/// the extension ask before the ping shares the limit, so an unreachable server still takes no longer.
async fn probe_within(address: &str, username: &str, password: &str, limit: Duration) -> Probe {
    tokio::time::timeout(limit, probe_once(address, username, password, limit)).await.unwrap_or(Probe::TimedOut)
}

async fn probe_once(address: &str, username: &str, password: &str, limit: Duration) -> Probe {
    let Some(address) = parse_address(address) else { return Probe::BadAddress };
    let login_given = !username.trim().is_empty() && !password.is_empty();
    let username = if login_given { username.trim() } else { "navia-probe" };
    let credentials = Credentials::new(username, if login_given { password } else { "" });
    let mut config = ClientConfig::new(address.url, credentials).read_only();
    config.timeout = limit;
    let Ok(client) = Client::new(config) else { return Probe::BadAddress };
    let Ok(request) = client.ping_request().await else { return Probe::BadAddress };
    match request.send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            match response.bytes().await {
                Ok(body) => classify_response(status, &body, username, login_given),
                Err(e) => classify_transport(&e),
            }
        }
        Err(e) => classify_transport(&e),
    }
}

/// Reads the answer, checking the kind of server before the login.
fn classify_response(status: u16, body: &[u8], username: &str, login_given: bool) -> Probe {
    if (500..600).contains(&status) {
        return Probe::Unreachable { detail: format!("The server answered with HTTP {status}.") };
    }
    let Some(response) = serde_json::from_slice::<Value>(body).ok().and_then(|v| v.get("subsonic-response").cloned()) else {
        return Probe::NotSubsonic;
    };
    let text = |key: &str| response.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    let kind = text("type");
    if !kind.as_deref().is_some_and(|k| k.eq_ignore_ascii_case("navidrome")) {
        return Probe::NotNavidrome { name: kind.map_or_else(|| "Subsonic".to_string(), |k| capitalized(&k)) };
    }
    let server = match text("serverVersion") {
        Some(version) => format!("Navidrome {version}"),
        None => "Navidrome".to_string(),
    };
    let api = match (response.get("openSubsonic").and_then(Value::as_bool).unwrap_or(false), text("version")) {
        (true, Some(version)) => format!("OpenSubsonic {version}"),
        (false, Some(version)) => format!("Subsonic {version}"),
        (_, None) => "Subsonic".to_string(),
    };
    if response.get("status").and_then(Value::as_str) == Some("ok") {
        return if login_given { Probe::Connected { server, api, username: username.to_string() } } else { Probe::Found { server, api } };
    }
    let error = response.get("error");
    let code = error.and_then(|e| e.get("code")).and_then(Value::as_i64).unwrap_or(0);
    let message = error.and_then(|e| e.get("message")).and_then(Value::as_str).unwrap_or("").to_string();
    match code {
        40 if login_given => Probe::WrongLogin,
        41 => Probe::TokenUnsupported,
        10 | 40 if !login_given => Probe::Found { server, api },
        _ => Probe::ServerError { code, message },
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Sorts a failed request into what to tell the user.
/// reqwest's URL is stripped before the error chain is searched, so the token never reaches any text.
fn classify_transport(error: &reqwest::Error) -> Probe {
    if error.is_timeout() {
        return Probe::TimedOut;
    }
    let mut chain = String::new();
    let mut source: Option<&dyn std::error::Error> = Some(error);
    while let Some(e) = source {
        chain.push_str(&e.to_string());
        chain.push(' ');
        source = e.source();
    }
    let chain = chain.to_lowercase();
    let contains = |needles: &[&str]| needles.iter().any(|n| chain.contains(n));
    if contains(&["certificate", "unknownissuer", "notvalidforname"]) {
        Probe::BadCertificate
    } else if contains(&["dns error", "failed to lookup", "name or service not known", "no such host", "nodename nor servname"]) {
        Probe::UnknownHost
    } else if contains(&["connection refused"]) {
        Probe::Refused
    } else if contains(&["tls", "ssl", "handshake", "corrupt message", "invalidcontenttype", "alert"]) {
        Probe::NotSecure
    } else {
        Probe::Unreachable { detail: "The connection didn't work. Check the address and your network.".to_string() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::path;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn parsed(raw: &str) -> Address {
        parse_address(raw).expect(raw)
    }

    #[test]
    fn addresses_get_a_scheme_and_lose_trailing_parts() {
        assert_eq!(parsed("music.example.org").url, "https://music.example.org");
        assert_eq!(parsed(" http://192.168.1.20:4533/ ").url, "http://192.168.1.20:4533");
        assert_eq!(parsed("https://example.org/navidrome/?x=1#y").url, "https://example.org/navidrome");
        assert_eq!(parsed("nas:4533").url, "https://nas:4533");
        assert_eq!(parsed("http://nas").url, "http://nas");
    }

    #[test]
    fn nonsense_is_not_an_address() {
        for raw in ["", "   ", "music", "ftp://example.org", "http://", "https://a b.org", "https://exa mple"] {
            assert_eq!(parse_address(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn only_remote_plain_http_warns() {
        assert!(parsed("http://music.example.org").unencrypted);
        assert!(!parsed("https://music.example.org").unencrypted);
        for local in ["http://localhost:4533", "http://127.0.0.1:4533", "http://192.168.1.2", "http://10.0.0.8", "http://172.16.3.4", "http://100.64.1.2", "http://nas.local", "http://[::1]:4533"] {
            assert!(!parsed(local).unencrypted, "{local}");
        }
        assert!(parsed("http://8.8.8.8").unencrypted);
    }

    fn ok_body(extra: &str, status: &str) -> String {
        format!(r#"{{"subsonic-response":{{"status":"{status}","version":"1.16.1","type":"navidrome","serverVersion":"0.64.2","openSubsonic":true{extra}}}}}"#)
    }

    async fn server(body: ResponseTemplate) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(path("/rest/ping")).respond_with(body).mount(&server).await;
        server
    }

    #[tokio::test]
    async fn a_good_login_reports_the_server() {
        let s = server(ResponseTemplate::new(200).set_body_string(ok_body("", "ok"))).await;
        assert_eq!(probe(&s.uri(), "ada", "pw").await, Probe::Connected { server: "Navidrome 0.64.2".into(), api: "OpenSubsonic 1.16.1".into(), username: "ada".into() });
    }

    #[tokio::test]
    async fn a_wrong_login_is_error_40() {
        let s = server(ResponseTemplate::new(200).set_body_string(ok_body(r#","error":{"code":40,"message":"Wrong username or password"}"#, "failed"))).await;
        assert_eq!(probe(&s.uri(), "ada", "pw").await, Probe::WrongLogin);
        assert!(matches!(probe(&s.uri(), "", "").await, Probe::Found { .. }));
    }

    #[tokio::test]
    async fn another_server_type_is_named() {
        let s = server(ResponseTemplate::new(200).set_body_string(r#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"gonic"}}"#)).await;
        assert_eq!(probe(&s.uri(), "ada", "pw").await, Probe::NotNavidrome { name: "Gonic".into() });
        let plain = server(ResponseTemplate::new(200).set_body_string(r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":40,"message":"x"}}}"#)).await;
        assert_eq!(probe(&plain.uri(), "ada", "pw").await, Probe::NotNavidrome { name: "Subsonic".into() });
    }

    #[tokio::test]
    async fn a_web_page_is_not_subsonic() {
        let s = server(ResponseTemplate::new(200).set_body_string("<html>Sign in</html>")).await;
        assert_eq!(probe(&s.uri(), "ada", "pw").await, Probe::NotSubsonic);
        let missing = server(ResponseTemplate::new(404).set_body_string("nope")).await;
        assert_eq!(probe(&missing.uri(), "ada", "pw").await, Probe::NotSubsonic);
    }

    #[tokio::test]
    async fn a_closed_port_is_refused() {
        // A port with nothing listening, bound briefly and released.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert_eq!(probe(&format!("http://127.0.0.1:{port}"), "ada", "pw").await, Probe::Refused);
    }

    #[tokio::test]
    async fn a_silent_server_times_out_within_the_limit() {
        let s = MockServer::start().await;
        Mock::given(wiremock::matchers::any()).respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30))).mount(&s).await;
        let started = std::time::Instant::now();
        assert_eq!(probe_within(&s.uri(), "ada", "pw", Duration::from_millis(800)).await, Probe::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn a_bad_address_never_goes_out() {
        assert_eq!(probe("not a url", "ada", "pw").await, Probe::BadAddress);
    }
}
