//! Whether a newer Navia is out, from the repository's latest GitHub release. For builds without
//! an updater of their own: the Mac until it has Developer ID signing, and Linux. Windows uses Velopack.

use std::time::Duration;

use semver::Version;
use serde::Deserialize;

use crate::{Error, Result};

pub const LATEST_RELEASE_API: &str = "https://api.github.com/repos/davekai/navia/releases/latest";
/// Opened when a release's own page link is missing or not https.
pub const RELEASES_PAGE: &str = "https://github.com/davekai/navia/releases/latest";
/// The automatic check runs at launch and then this often while Navia runs.
pub const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// Forces the automatic check in runs that otherwise skip it (`NAVIA_CHECK_UPDATES=1`).
pub const FORCE_ENV: &str = "NAVIA_CHECK_UPDATES";
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReleaseCheck {
    UpToDate,
    /// `version` is plain ("0.1.2"); `url` is the release's page, for the Download button.
    Available { version: String, url: String },
}

#[derive(Deserialize)]
struct LatestRelease {
    tag_name: String,
    #[serde(default)]
    html_url: Option<String>,
}

/// A version as tags and builds write it: "v0.1.2", "0.1.0+672", or a label like "0.1.0+672 (f6e8534)".
fn parse_version(text: &str) -> Option<Version> {
    let text = text.trim();
    let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
    Version::parse(text.split_whitespace().next()?).ok()
}

/// Compares GitHub's answer for the latest release with the running version, by SemVer precedence
/// (build metadata is ignored, so a test build "0.1.1+5" is up to date with 0.1.1).
pub fn compare(json: &str, running: &str) -> Result<ReleaseCheck> {
    let decode = |detail: String| Error::Decode { endpoint: "releases/latest".into(), detail };
    let release: LatestRelease = serde_json::from_str(json).map_err(|e| decode(e.to_string()))?;
    let latest = parse_version(&release.tag_name).ok_or_else(|| decode(format!("tag {:?} isn't a version", release.tag_name)))?;
    let current = parse_version(running).ok_or_else(|| decode(format!("running version {running:?} isn't a version")))?;
    if latest.cmp_precedence(&current).is_le() {
        return Ok(ReleaseCheck::UpToDate);
    }
    let url = release.html_url.filter(|url| url.starts_with("https://")).unwrap_or_else(|| RELEASES_PAGE.to_string());
    Ok(ReleaseCheck::Available { version: Version { build: semver::BuildMetadata::EMPTY, ..latest }.to_string(), url })
}

/// Asks GitHub for the latest release, blocking: call it off the UI thread.
pub fn check_latest_release(running: &str) -> Result<ReleaseCheck> {
    check_release_at(LATEST_RELEASE_API, running)
}

/// `check_latest_release` against another address (tests).
pub fn check_release_at(api: &str, running: &str) -> Result<ReleaseCheck> {
    let http = reqwest::blocking::Client::builder().timeout(TIMEOUT).user_agent(crate::client::user_agent()).build()?;
    let response = http.get(api).header(reqwest::header::ACCEPT, "application/vnd.github+json").send()?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::HttpStatus { status: status.as_u16(), endpoint: "releases/latest".into() });
    }
    compare(&response.text()?, running)
}

/// Whether this run may check by itself. Test, scripted and read-only runs and the developer
/// override don't, unless `NAVIA_CHECK_UPDATES=1` forces it.
pub fn automatic_check_allowed(test_run: bool) -> bool {
    if std::env::var_os(FORCE_ENV).is_some_and(|v| v == "1") {
        return true;
    }
    !test_run && !crate::connect::developer_override()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPLY: &str = r#"{"url":"https://api.github.com/repos/davekai/navia/releases/1","html_url":"https://github.com/davekai/navia/releases/tag/v0.1.2","tag_name":"v0.1.2","name":"Navia 0.1.2","draft":false,"prerelease":false,"assets":[]}"#;

    fn available() -> ReleaseCheck {
        ReleaseCheck::Available { version: "0.1.2".into(), url: "https://github.com/davekai/navia/releases/tag/v0.1.2".into() }
    }

    #[test]
    fn a_newer_tag_is_available_and_the_same_or_older_is_up_to_date() {
        assert_eq!(compare(REPLY, "0.1.1").unwrap(), available());
        assert_eq!(compare(REPLY, "0.1.2").unwrap(), ReleaseCheck::UpToDate);
        assert_eq!(compare(REPLY, "0.2.0").unwrap(), ReleaseCheck::UpToDate);
        assert_eq!(compare(REPLY, "0.1.10").unwrap(), ReleaseCheck::UpToDate);
    }

    #[test]
    fn test_builds_compare_by_their_release_version() {
        let latest = REPLY.replace("0.1.2", "0.1.1");
        assert!(matches!(compare(&latest, "0.1.0+672").unwrap(), ReleaseCheck::Available { version, .. } if version == "0.1.1"));
        assert_eq!(compare(&latest, "0.1.1+5").unwrap(), ReleaseCheck::UpToDate);
        assert_eq!(compare(&latest, "0.1.1+5 (26a806a)").unwrap(), ReleaseCheck::UpToDate);
    }

    #[test]
    fn a_reply_without_a_usable_page_link_opens_the_releases_page() {
        let reply = r#"{"tag_name":"0.3.0","html_url":"javascript:alert(1)"}"#;
        assert_eq!(compare(reply, "0.1.1").unwrap(), ReleaseCheck::Available { version: "0.3.0".into(), url: RELEASES_PAGE.into() });
        assert_eq!(compare(r#"{"tag_name":"v1.0.0"}"#, "0.1.1").unwrap(), ReleaseCheck::Available { version: "1.0.0".into(), url: RELEASES_PAGE.into() });
    }

    #[test]
    fn a_tag_or_reply_that_isnt_a_version_fails() {
        assert!(compare(r#"{"tag_name":"nightly"}"#, "0.1.1").is_err());
        assert!(compare(r#"{"message":"Not Found"}"#, "0.1.1").is_err());
        assert!(compare("<html>", "0.1.1").is_err());
        assert!(compare(REPLY, "?").is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn asks_github_with_navias_user_agent() {
        use wiremock::matchers::{header, method, path};
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(method("GET"))
            .and(path("/repos/davekai/navia/releases/latest"))
            .and(header("user-agent", crate::client::user_agent().as_str()))
            .and(header("accept", "application/vnd.github+json"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(REPLY))
            .expect(1)
            .mount(&server)
            .await;
        let api = format!("{}/repos/davekai/navia/releases/latest", server.uri());
        let found = tokio::task::spawn_blocking(move || check_release_at(&api, "0.1.1")).await.unwrap().unwrap();
        assert_eq!(found, available());
        let missing = format!("{}/nothing", server.uri());
        let failed = tokio::task::spawn_blocking(move || check_release_at(&missing, "0.1.1")).await.unwrap();
        assert!(matches!(failed, Err(Error::HttpStatus { status: 404, .. })));
    }
}
