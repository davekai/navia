//! The crate's error type. No variant carries a URL, because the Subsonic token and salt travel in query strings.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    /// Transport failure. The URL is stripped so credentials can never appear here.
    #[error("network error talking to the server: {0}")]
    Network(String),

    /// The server answered with a non-success HTTP status.
    #[error("server returned HTTP {status} for {endpoint}")]
    HttpStatus { status: u16, endpoint: String },

    /// The server answered with a Subsonic error document.
    #[error("server error {code} from {endpoint}: {message}")]
    Subsonic { code: i32, endpoint: String, message: String },

    /// not sent: the server limited or failed an earlier request of this kind, and its wait isn't over.
    #[error("waiting {seconds} s before asking the server for {endpoint} again")]
    Waiting { endpoint: String, seconds: u64 },

    /// The response was not what the protocol promises.
    #[error("could not understand the server's response to {endpoint}: {detail}")]
    Decode { endpoint: String, detail: String },

    /// A call that changes server state was refused locally because the client is read-only.
    #[error("{endpoint} changes server state and this client is read-only")]
    WritesDisabled { endpoint: String },

    #[error("invalid server address: {0}")]
    InvalidUrl(String),

    #[error("local database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("local database task failed: {0}")]
    DatabaseTask(String),

    #[error("no per-user data directory is available on this system")]
    NoDataDirectory,

    #[error("sync stopped: {0}")]
    Sync(String),

    #[error("audio error: {0}")]
    Audio(String),

    /// An AirPlay receiver refused, timed out or went away.
    #[error("AirPlay: {0}")]
    AirPlay(String),

    /// A stream was canceled on purpose, and whoever canceled it handles what comes next.
    #[error("cancelled")]
    Cancelled,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        // `without_url` is what keeps the token out of logs and error messages.
        Error::Network(e.without_url().to_string())
    }
}

/// Well-known Subsonic error codes.
pub mod codes {
    pub const GENERIC: i32 = 0;
    pub const MISSING_PARAMETER: i32 = 10;
    pub const WRONG_CREDENTIALS: i32 = 40;
    pub const TOKEN_AUTH_UNSUPPORTED: i32 = 41;
    pub const NOT_AUTHORIZED: i32 = 50;
    pub const NOT_FOUND: i32 = 70;
}

impl Error {
    /// True when the server rejected the login, so retrying is pointless.
    pub fn is_auth_failure(&self) -> bool {
        matches!(self, Error::Subsonic { code, .. } if matches!(*code, codes::WRONG_CREDENTIALS | codes::TOKEN_AUTH_UNSUPPORTED))
    }

    /// True when the server itself is unreachable, not when one track has a problem.
    /// Playback waits out an outage instead of moving on.
    pub fn is_outage(&self) -> bool {
        match self {
            Error::Network(_) => true,
            Error::HttpStatus { status, .. } => matches!(*status, 502..=504),
            _ => false,
        }
    }

    /// True when the failure is one that a later retry could fix.
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Network(_) | Error::Waiting { .. } => true,
            Error::HttpStatus { status, .. } => *status >= 500 || *status == 429 || *status == 408,
            _ => false,
        }
    }
}
