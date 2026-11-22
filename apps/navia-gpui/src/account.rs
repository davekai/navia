//! The signed-in account. The address and username live in `account.json` and the password
//! in the system keyring. With `NAVIA_DEV=1`, the `NAVIDROME_*` environment variables win.
//!
//! The password is never written anywhere else, logged or shown. Without a keyring it's kept
//! for this run only.

use crate::i18n::{tr, trf, n_};
use std::path::PathBuf;

use navia_core::client::Credentials;
use serde_json::{Value, json};

use crate::core_library::Connection;

/// the entry's name is `user@url`.
const SERVICE: &str = "navia";
pub const NO_KEYRING: &str = n_("No system keyring was found, so Navia can't remember your password.");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedAccount {
    pub url: String,
    pub user: String,
}

/// shown instead of starting the app.
#[derive(Clone, Debug, Default)]
pub struct SignInPrompt {
    pub account: Option<SavedAccount>,
    /// Why it's asking again, unless this is the first run.
    pub note: Option<(&'static str, String)>,
}

fn saved_path() -> Option<PathBuf> {
    crate::persist::config_dir().map(|d| d.join("account.json"))
}

pub fn load_saved() -> Option<SavedAccount> {
    let value: Value = serde_json::from_slice(&std::fs::read(saved_path()?).ok()?).ok()?;
    Some(SavedAccount { url: value.get("url")?.as_str()?.to_string(), user: value.get("user")?.as_str()?.to_string() })
}

pub fn save_account(url: &str, user: &str) {
    let Some(path) = saved_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let body = serde_json::to_vec_pretty(&json!({ "url": url, "user": user })).unwrap_or_default();
    let _ = write_private(&path, &body);
}

pub fn clear_saved() {
    if let Some(path) = saved_path() {
        let _ = std::fs::remove_file(path);
    }
}

fn write_private(path: &std::path::Path, body: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        file.write_all(body)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, body)
    }
}

pub enum SecretRead {
    Found(String),
    Missing,
    NoKeyring,
}

fn entry_name(url: &str, user: &str) -> String {
    format!("{user}@{}", url.trim_end_matches('/'))
}

/// Test overrides. `NAVIA_TEST_NO_KEYRING=1` acts like a desktop without a secret service, and
/// `NAVIA_TEST_SECRET_FILE` stores secrets in that file so tests never touch a real keyring.
fn test_mode() -> TestSecrets {
    if std::env::var_os("NAVIA_TEST_NO_KEYRING").is_some_and(|v| v == "1") {
        return TestSecrets::None;
    }
    match std::env::var_os("NAVIA_TEST_SECRET_FILE") {
        Some(file) => TestSecrets::File(PathBuf::from(file)),
        None => TestSecrets::Real,
    }
}

enum TestSecrets {
    Real,
    None,
    File(PathBuf),
}

fn read_test_file(path: &std::path::Path) -> Value {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| json!({}))
}

pub fn read_secret(url: &str, user: &str) -> SecretRead {
    let name = entry_name(url, user);
    match test_mode() {
        TestSecrets::None => return SecretRead::NoKeyring,
        TestSecrets::File(path) => {
            return read_test_file(&path).get(&name).and_then(Value::as_str).map_or(SecretRead::Missing, |s| SecretRead::Found(s.to_string()));
        }
        TestSecrets::Real => {}
    }
    match keyring::Entry::new(SERVICE, &name).and_then(|e| e.get_password()) {
        Ok(password) => SecretRead::Found(password),
        Err(keyring::Error::NoEntry) => SecretRead::Missing,
        Err(e) => {
            eprintln!("navia: the system keyring isn't available: {}", keyring_problem(&e));
            SecretRead::NoKeyring
        }
    }
}

/// the error text says why it couldn't be saved.
pub fn write_secret(url: &str, user: &str, password: &str) -> Result<(), String> {
    let name = entry_name(url, user);
    match test_mode() {
        TestSecrets::None => return Err(tr(NO_KEYRING).to_string()),
        TestSecrets::File(path) => {
            let mut all = read_test_file(&path);
            all[name] = json!(password);
            return write_private(&path, &serde_json::to_vec(&all).unwrap_or_default()).map_err(|e| e.to_string());
        }
        TestSecrets::Real => {}
    }
    keyring::Entry::new(SERVICE, &name).and_then(|e| e.set_password(password)).map_err(|e| {
        eprintln!("navia: couldn't save the password to the system keyring: {}", keyring_problem(&e));
        tr(NO_KEYRING).to_string()
    })
}

/// does nothing if there isn't one.
pub fn forget_secret(url: &str, user: &str) {
    let name = entry_name(url, user);
    match test_mode() {
        TestSecrets::None => return,
        TestSecrets::File(path) => {
            let mut all = read_test_file(&path);
            if let Some(map) = all.as_object_mut() {
                map.remove(&name);
            }
            let _ = write_private(&path, &serde_json::to_vec(&all).unwrap_or_default());
            return;
        }
        TestSecrets::Real => {}
    }
    if let Ok(entry) = keyring::Entry::new(SERVICE, &name) {
        let _ = entry.delete_credential();
    }
}

/// A short problem kind. Never the error's text, which could quote the secret's attributes.
fn keyring_problem(e: &keyring::Error) -> &'static str {
    match e {
        keyring::Error::NoStorageAccess(_) => tr("no access to the secret service"),
        keyring::Error::NoDefaultStore => tr("no secret service found"),
        keyring::Error::PlatformFailure(_) => tr("the keyring reported a failure"),
        _ => tr("the keyring refused the request"),
    }
}

/// The login to start with. The developer environment override, else the saved account.
pub fn resolve() -> Result<Connection, SignInPrompt> {
    if let Some(connection) = Connection::from_env() {
        return Ok(connection);
    }
    let Some(account) = load_saved() else { return Err(SignInPrompt::default()) };
    match read_secret(&account.url, &account.user) {
        SecretRead::Found(password) => Ok(Connection::saved(account.url, account.user, password)),
        SecretRead::Missing => Err(SignInPrompt { note: Some((tr("Enter your password to continue"), trf("Navia doesn't have the password for {user} on this device.", &[("user", &account.user)]))), account: Some(account) }),
        SecretRead::NoKeyring => Err(SignInPrompt { note: Some((tr("Enter your password to continue"), tr("No system keyring was found, so Navia can't remember your password. It asks again each time it starts.").to_string())), account: Some(account) }),
    }
}

pub fn account_key(url: &str, user: &str) -> String {
    Credentials::new(user, "").account_key(url)
}

/// Keyring storage for secrets other than the password, such as AirPlay's identity key.
/// An `Err` makes the core fall back to a file only this user can read.
pub struct KeyringSecrets;

impl navia_core::audio::airplay::SecretStore for KeyringSecrets {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        match test_mode() {
            TestSecrets::None => Err("no keyring".into()),
            TestSecrets::File(path) => Ok(read_test_file(&path).get(key).and_then(Value::as_str).map(str::to_string)),
            TestSecrets::Real => match keyring::Entry::new(SERVICE, key).and_then(|e| e.get_password()) {
                Ok(v) => Ok(Some(v)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(keyring_problem(&e).to_string()),
            },
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        match test_mode() {
            TestSecrets::None => Err("no keyring".into()),
            TestSecrets::File(path) => {
                let mut all = read_test_file(&path);
                all[key] = json!(value);
                write_private(&path, &serde_json::to_vec(&all).unwrap_or_default()).map_err(|e| e.to_string())
            }
            TestSecrets::Real => keyring::Entry::new(SERVICE, key).and_then(|e| e.set_password(value)).map_err(|e| keyring_problem(&e).to_string()),
        }
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        match test_mode() {
            TestSecrets::None => Err("no keyring".into()),
            TestSecrets::File(path) => {
                let mut all = read_test_file(&path);
                if let Some(map) = all.as_object_mut() {
                    map.remove(key);
                }
                write_private(&path, &serde_json::to_vec(&all).unwrap_or_default()).map_err(|e| e.to_string())
            }
            TestSecrets::Real => match keyring::Entry::new(SERVICE, key).and_then(|e| e.delete_credential()) {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(keyring_problem(&e).to_string()),
            },
        }
    }
}
