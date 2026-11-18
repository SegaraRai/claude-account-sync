//! Saved sign-ins of the desktop app, one per account, and switching between them.
//!
//! The app keeps its sign-in in two places in its data folder:
//!
//! - the Chromium cookie database `Network/Cookies` (`Cookies` in older versions), which holds the claude.ai
//!   session;
//! - the `oauth:tokenCache` and `oauth:tokenCacheV2` fields of `config.json`, which hold the OAuth tokens. The
//!   field `lastKnownAccountUuid` names the account.
//!
//! The app encrypts both with a key that the operating system protects for the current user (DPAPI on Windows,
//! the keychain on macOS). This module never decrypts them. It copies the encrypted bytes into a profile folder and
//! back, which works only for the same user on the same computer.
//!
//! The app locks the cookie database while it runs, and it keeps the sign-in in memory. A profile is therefore
//! saved and restored only while the app is stopped.
//!
//! Signing out in the app most likely revokes the session on the server, which makes the saved copy of that
//! account useless. [`ProfileStore::mark_signed_out`] records this, and the user has to sign in to that account
//! again. Switching with [`switch::switch`] does not sign out, so the saved sign-ins of other accounts stay valid.
//!
//! - [`switch`]: stops the app, saves the current sign-in, restores another one, and starts the app.
//! - [`picker`]: asks the user which account to use, in a window or in the terminal.
//! - [`observer`]: lets the watcher save sign-ins, record when each account was used, and react to a sign-out.

pub mod observer;
pub mod picker;
pub mod switch;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::file_ops::{self, Backup, read_regular, write_atomic};
use crate::replica::is_uuid;

/// The fields of the app's `config.json` that hold the OAuth tokens.
pub const OAUTH_KEYS: [&str; 2] = ["oauth:tokenCacheV2", "oauth:tokenCache"];

/// The field of the app's `config.json` that names the signed-in account.
const ACCOUNT_KEY: &str = "lastKnownAccountUuid";

/// The cookie database and the SQLite side files that can belong to it.
const COOKIE_FILES: [&str; 4] = ["Cookies", "Cookies-journal", "Cookies-wal", "Cookies-shm"];

/// Largest file that this module reads from the app's folder.
const MAX_FILE_LEN: u64 = 256 << 20;

const PROFILE_VERSION: u32 = 1;

/// Errors from saving or restoring a sign-in.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("cannot access {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{path} is not valid JSON: {source}")]
    Invalid {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("the app is not signed in")]
    NotSignedIn,
    #[error("{0:?} is not an account UUID")]
    InvalidAccount(String),
    #[error("no saved sign-in for account {0}")]
    NoSnapshot(String),
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> ProfileError + '_ {
    move |source| ProfileError::Io {
        path: path.to_owned(),
        source,
    }
}

/// Which account the app shows and whether it holds a sign-in, read from `config.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginState {
    /// `lastKnownAccountUuid`. The app may keep this field after it signs out.
    pub account: Option<String>,
    /// `true` when one of [`OAUTH_KEYS`] holds a non-empty value.
    pub signed_in: bool,
}

impl LoginState {
    /// Reads the state from the app's data folder. A missing or unreadable `config.json` means not signed in.
    pub fn read(data_dir: &Path) -> Self {
        match read_app_config(data_dir) {
            Ok(Some(config)) => Self::from_config(&config),
            _ => Self::default(),
        }
    }

    fn from_config(config: &Map<String, Value>) -> Self {
        Self {
            account: config
                .get(ACCOUNT_KEY)
                .and_then(Value::as_str)
                .filter(|account| is_uuid(account))
                .map(str::to_owned),
            signed_in: OAUTH_KEYS.iter().any(|key| {
                config
                    .get(*key)
                    .is_some_and(|value| value.as_str().is_some_and(|text| !text.is_empty()))
            }),
        }
    }

    /// Returns the signed-in account, or `None` when the app is not signed in.
    pub fn signed_in_account(&self) -> Option<&str> {
        self.account.as_deref().filter(|_| self.signed_in)
    }
}

fn read_app_config(data_dir: &Path) -> Result<Option<Map<String, Value>>, ProfileError> {
    let path = data_dir.join("config.json");
    let Some(bytes) = read_regular(&path, 64 << 20).map_err(io_error(&path))? else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|source| ProfileError::Invalid { path, source })
}

/// The metadata of one saved account: `profiles/<account>/profile.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    #[serde(default = "profile_version")]
    pub version: u32,
    pub account: String,
    /// A name that the user gave the account. The app stores no readable name or email address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// When the sign-in was saved, in Unix milliseconds. `None` when no sign-in is saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<i64>,
    /// When the app last showed this account while signed in, in Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_at: Option<i64>,
    /// When the app signed out of this account after the sign-in was saved. The saved sign-in is then most likely
    /// revoked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_out_at: Option<i64>,
    /// The folder of the cookie database, relative to the app's data folder: `Network` or empty.
    #[serde(default)]
    pub cookies_dir: String,
    /// The hash of the saved files, which tells whether a new save would change them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// The account's name, email address, and organization, from the Claude Code CLI's profile cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Identity>,
}

/// An account's name and email address.
///
/// The desktop app stores neither in a readable form. The Claude Code CLI that the app starts for Code tab
/// sessions caches the profile of the account that it signs in with in `oauthAccount` of `~/.claude.json`
/// (`$CLAUDE_CONFIG_DIR/.claude.json` when that is set). That cache holds one account at a time, so the watcher
/// copies it into the matching profile whenever it changes, and each account's identity becomes known after its
/// first Code tab session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_name: Option<String>,
}

/// Reads the account UUID and identity from the Claude Code CLI's profile cache. Returns `None` when the file,
/// the cache, or a valid account UUID is missing.
pub fn read_cli_identity() -> Option<(String, Identity)> {
    // The CLI keeps this file next to its config folder, `~/.claude.json`, or inside `$CLAUDE_CONFIG_DIR`.
    let path = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join(".claude.json"),
        _ => dirs::home_dir()?.join(".claude.json"),
    };
    let bytes = read_regular(&path, 64 << 20).ok()??;
    let config: Value = serde_json::from_slice(&bytes).ok()?;
    let account = config.get("oauthAccount")?;
    let text = |key: &str| {
        account
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let uuid = text("accountUuid").filter(|uuid| is_uuid(uuid))?;
    let identity = Identity {
        display_name: text("displayName"),
        email: text("emailAddress"),
        organization_name: text("organizationName"),
    };
    (identity.display_name.is_some() || identity.email.is_some()).then_some((uuid, identity))
}

fn profile_version() -> u32 {
    PROFILE_VERSION
}

impl Profile {
    fn new(account: &str) -> Self {
        Self {
            version: PROFILE_VERSION,
            account: account.to_owned(),
            label: None,
            saved_at: None,
            last_active_at: None,
            signed_out_at: None,
            cookies_dir: String::new(),
            content_hash: None,
            identity: None,
        }
    }

    /// Returns `true` when a saved sign-in exists and the app has not signed out of it since.
    pub fn can_restore(&self) -> bool {
        match (self.saved_at, self.signed_out_at) {
            (Some(saved), Some(signed_out)) => saved > signed_out,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }

    /// Returns the label, or the first block of the account UUID.
    /// Returns the label, the account's display name, its email address, or the first block of the account UUID,
    /// whichever exists first.
    pub fn display_name(&self) -> String {
        let identity = self.identity.as_ref();
        self.label
            .clone()
            .or_else(|| identity.and_then(|identity| identity.display_name.clone()))
            .or_else(|| identity.and_then(|identity| identity.email.clone()))
            .unwrap_or_else(|| format!("account {}", &self.account[..8.min(self.account.len())]))
    }

    /// Returns the account's email address when it is known.
    pub fn email(&self) -> Option<&str> {
        self.identity.as_ref()?.email.as_deref()
    }
}

/// The result of [`ProfileStore::capture`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureOutcome {
    Saved,
    Unchanged,
}

/// The `profiles` folder in the state folder.
#[derive(Clone, Debug)]
pub struct ProfileStore {
    pub root: PathBuf,
}

/// The sign-in files read from the app's folder, before they are written to a profile.
struct SignIn {
    oauth: Map<String, Value>,
    cookies_dir: String,
    cookies: Vec<(String, Vec<u8>)>,
}

impl SignIn {
    fn hash(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(serde_json::to_vec(&self.oauth).expect("JSON serializes"));
        hasher.update(self.cookies_dir.as_bytes());
        for (name, bytes) in &self.cookies {
            hasher.update(name.as_bytes());
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

impl ProfileStore {
    pub fn new(state_root: &Path) -> Self {
        Self {
            root: state_root.join("profiles"),
        }
    }

    fn dir(&self, account: &str) -> Result<PathBuf, ProfileError> {
        if !is_uuid(account) {
            return Err(ProfileError::InvalidAccount(account.to_owned()));
        }
        Ok(self.root.join(account))
    }

    /// Returns the profile of `account`, or `None` when none exists.
    pub fn load(&self, account: &str) -> Result<Option<Profile>, ProfileError> {
        let path = self.dir(account)?.join("profile.json");
        let Some(bytes) = read_regular(&path, 1 << 20).map_err(io_error(&path))? else {
            return Ok(None);
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|source| ProfileError::Invalid { path, source })
    }

    fn load_or_new(&self, account: &str) -> Result<Profile, ProfileError> {
        Ok(self.load(account)?.unwrap_or_else(|| Profile::new(account)))
    }

    fn save_meta(&self, profile: &Profile) -> Result<(), ProfileError> {
        let dir = self.dir(&profile.account)?;
        create_private_dir(&dir).map_err(io_error(&dir))?;
        let path = dir.join("profile.json");
        let mut bytes = serde_json::to_vec_pretty(profile).expect("profile serializes");
        bytes.push(b'\n');
        write_atomic(&path, &bytes, None).map_err(io_error(&path))
    }

    /// Lists the profiles, most recently used first. Folders that are not valid profiles are skipped.
    pub fn list(&self) -> Result<Vec<Profile>, ProfileError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(ProfileError::Io {
                    path: self.root.clone(),
                    source,
                });
            }
        };
        let mut profiles = Vec::new();
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_uuid(&name) {
                continue;
            }
            match self.load(&name) {
                Ok(Some(profile)) => profiles.push(profile),
                Ok(None) => {}
                Err(error) => log::warn!("skipped a profile: {error}"),
            }
        }
        profiles.sort_by(|a, b| {
            b.last_active_at
                .cmp(&a.last_active_at)
                .then_with(|| a.account.cmp(&b.account))
        });
        Ok(profiles)
    }

    /// Saves the app's current sign-in as the profile of `account`. The app must be stopped.
    ///
    /// Returns [`ProfileError::NotSignedIn`] when the app holds no OAuth token, so that a signed-out state never
    /// replaces a saved sign-in.
    pub fn capture(
        &self,
        data_dir: &Path,
        account: &str,
        now: SystemTime,
    ) -> Result<CaptureOutcome, ProfileError> {
        let dir = self.dir(account)?;
        let config = read_app_config(data_dir)?.ok_or(ProfileError::NotSignedIn)?;
        if !LoginState::from_config(&config).signed_in {
            return Err(ProfileError::NotSignedIn);
        }
        let sign_in = read_sign_in(data_dir, &config)?;
        let hash = sign_in.hash();
        let mut profile = self.load_or_new(account)?;
        if profile.can_restore() && profile.content_hash.as_deref() == Some(hash.as_str()) {
            return Ok(CaptureOutcome::Unchanged);
        }

        create_private_dir(&dir).map_err(io_error(&dir))?;
        let oauth_path = dir.join("oauth.json");
        let oauth_bytes = serde_json::to_vec(&sign_in.oauth).expect("JSON serializes");
        write_atomic(&oauth_path, &oauth_bytes, None).map_err(io_error(&oauth_path))?;
        for name in COOKIE_FILES {
            let path = dir.join(name);
            match sign_in.cookies.iter().find(|(n, _)| n == name) {
                Some((_, bytes)) => write_atomic(&path, bytes, None).map_err(io_error(&path))?,
                None => remove_if_exists(&path)?,
            }
        }
        profile.saved_at = Some(file_ops::unix_millis(now));
        profile.signed_out_at = None;
        profile.cookies_dir = sign_in.cookies_dir;
        profile.content_hash = Some(hash);
        self.save_meta(&profile)?;
        Ok(CaptureOutcome::Saved)
    }

    /// Writes the saved sign-in of `account` into the app's folder. The app must be stopped.
    ///
    /// Copies the files that this replaces into `backup`. Other fields of `config.json` stay as they are.
    pub fn restore(
        &self,
        data_dir: &Path,
        account: &str,
        backup: &Backup,
    ) -> Result<(), ProfileError> {
        let dir = self.dir(account)?;
        let profile = self
            .load(account)?
            .filter(Profile::can_restore)
            .ok_or_else(|| ProfileError::NoSnapshot(account.to_owned()))?;
        let oauth_path = dir.join("oauth.json");
        let oauth: Map<String, Value> =
            match read_regular(&oauth_path, 1 << 20).map_err(io_error(&oauth_path))? {
                Some(bytes) => {
                    serde_json::from_slice(&bytes).map_err(|source| ProfileError::Invalid {
                        path: oauth_path.clone(),
                        source,
                    })?
                }
                None => return Err(ProfileError::NoSnapshot(account.to_owned())),
            };
        let mut cookies = Vec::new();
        for name in COOKIE_FILES {
            let path = dir.join(name);
            if let Some(bytes) = read_regular(&path, MAX_FILE_LEN).map_err(io_error(&path))? {
                cookies.push((name, bytes));
            }
        }
        if !cookies.iter().any(|(name, _)| *name == "Cookies") {
            return Err(ProfileError::NoSnapshot(account.to_owned()));
        }

        write_sign_in(
            data_dir,
            &profile.cookies_dir,
            Some((&oauth, account)),
            &cookies,
            backup,
        )
    }

    /// Removes the sign-in from the app's folder without signing out on the server, so that the app shows its
    /// sign-in page and another account can sign in. The app must be stopped.
    ///
    /// Removes the OAuth tokens and `lastKnownAccountUuid` from `config.json` and deletes the cookie database,
    /// after copying them into `backup`. Save the current sign-in with [`ProfileStore::capture`] first, or it is
    /// lost.
    pub fn clear_sign_in(data_dir: &Path, backup: &Backup) -> Result<(), ProfileError> {
        write_sign_in(data_dir, &cookies_dir_name(data_dir), None, &[], backup)
    }

    /// Records that the app shows `account` while signed in. Creates the profile when it is missing.
    pub fn touch_active(&self, account: &str, now: SystemTime) -> Result<(), ProfileError> {
        let mut profile = self.load_or_new(account)?;
        profile.last_active_at = Some(file_ops::unix_millis(now));
        self.save_meta(&profile)
    }

    /// Stores `identity` in the profile of `account` when it differs. Creates the profile when it is missing.
    pub fn record_identity(
        &self,
        account: &str,
        identity: &Identity,
    ) -> Result<bool, ProfileError> {
        let mut profile = self.load_or_new(account)?;
        if profile.identity.as_ref() == Some(identity) {
            return Ok(false);
        }
        profile.identity = Some(identity.clone());
        self.save_meta(&profile)?;
        Ok(true)
    }

    /// Records that the app signed out of `account`, which most likely revoked its saved sign-in.
    pub fn mark_signed_out(&self, account: &str, now: SystemTime) -> Result<(), ProfileError> {
        let Some(mut profile) = self.load(account)? else {
            return Ok(());
        };
        if profile.saved_at.is_some() {
            profile.signed_out_at = Some(file_ops::unix_millis(now));
            self.save_meta(&profile)?;
        }
        Ok(())
    }

    /// Sets or, with `None`, removes the label of `account`.
    pub fn set_label(&self, account: &str, label: Option<String>) -> Result<(), ProfileError> {
        let mut profile = self
            .load(account)?
            .ok_or_else(|| ProfileError::NoSnapshot(account.to_owned()))?;
        profile.label = label.filter(|label| !label.trim().is_empty());
        self.save_meta(&profile)
    }

    /// Deletes the profile of `account`, including its saved sign-in.
    pub fn forget(&self, account: &str) -> Result<(), ProfileError> {
        let dir = self.dir(account)?;
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Err(ProfileError::NoSnapshot(account.to_owned()))
            }
            Err(source) => Err(ProfileError::Io { path: dir, source }),
        }
    }

    /// Finds the profile that `query` names: a full account UUID, the start of one (at least 4 characters), or the
    /// name that [`Profile::display_name`] shows or the email address, ignoring case.
    pub fn resolve(&self, query: &str) -> anyhow::Result<Profile> {
        let profiles = self.list()?;
        let lower = query.to_lowercase();
        if let Some(profile) = profiles.iter().find(|p| p.account == lower) {
            return Ok(profile.clone());
        }
        let by_label: Vec<_> = profiles
            .iter()
            .filter(|p| {
                p.display_name().to_lowercase() == lower
                    || p.email().is_some_and(|email| email.to_lowercase() == lower)
            })
            .collect();
        let candidates = if by_label.is_empty() && lower.len() >= 4 {
            profiles
                .iter()
                .filter(|p| p.account.starts_with(&lower))
                .collect()
        } else {
            by_label
        };
        match candidates.as_slice() {
            [profile] => Ok((*profile).clone()),
            [] => anyhow::bail!("no saved account matches {query:?}; see `profiles`"),
            _ => anyhow::bail!("{query:?} matches several accounts; use the account UUID"),
        }
    }
}

/// Replaces the sign-in in the app's folder. `oauth` holds the token fields and the account to write, or `None` to
/// remove them. The cookie files in `cookies_dir` become exactly `cookies`. Copies the replaced files into `backup`
/// first, so that a failure part way leaves a complete copy of the previous state. Other fields of `config.json`
/// stay as they are.
fn write_sign_in(
    data_dir: &Path,
    cookies_dir: &str,
    oauth: Option<(&Map<String, Value>, &str)>,
    cookies: &[(&str, Vec<u8>)],
    backup: &Backup,
) -> Result<(), ProfileError> {
    let config_path = data_dir.join("config.json");
    let config_bytes = read_regular(&config_path, 64 << 20).map_err(io_error(&config_path))?;
    let cookies_dir = data_dir.join(cookies_dir);
    let backup_error = |source| ProfileError::Io {
        path: backup.root().to_owned(),
        source,
    };
    if let Some(bytes) = &config_bytes {
        backup
            .save(Path::new("sign-in/config.json"), bytes)
            .map_err(backup_error)?;
    }
    for name in COOKIE_FILES {
        let path = cookies_dir.join(name);
        if let Some(bytes) = read_regular(&path, MAX_FILE_LEN).map_err(io_error(&path))? {
            backup
                .save(&Path::new("sign-in").join(name), &bytes)
                .map_err(backup_error)?;
        }
    }

    let mut config: Map<String, Value> = match &config_bytes {
        Some(bytes) => serde_json::from_slice(bytes).map_err(|source| ProfileError::Invalid {
            path: config_path.clone(),
            source,
        })?,
        None => Map::new(),
    };
    for key in OAUTH_KEYS {
        match oauth.and_then(|(oauth, _)| oauth.get(key)) {
            Some(value) => config.insert(key.to_owned(), value.clone()),
            None => config.remove(key),
        };
    }
    match oauth {
        Some((_, account)) => {
            config.insert(ACCOUNT_KEY.to_owned(), Value::String(account.to_owned()))
        }
        None => config.remove(ACCOUNT_KEY),
    };

    for name in COOKIE_FILES {
        let path = cookies_dir.join(name);
        match cookies.iter().find(|(n, _)| *n == name) {
            Some((_, bytes)) => write_atomic(&path, bytes, None).map_err(io_error(&path))?,
            None => remove_if_exists(&path)?,
        }
    }
    let mut bytes = serde_json::to_vec_pretty(&config).expect("JSON serializes");
    bytes.push(b'\n');
    write_atomic(&config_path, &bytes, None).map_err(io_error(&config_path))
}

/// Starts the CLI in the background with `--data-dir` and `--state-dir` and then `args`, without a console window.
/// The agent and the tray use this to open windows that ask the user. Failures are logged.
pub fn spawn_cli(data_dir: &Path, state_dir: &crate::state::StateDir, args: &[&str]) {
    let Some(cli) = cli_executable() else {
        log::warn!("cannot find the claude-account-sync executable");
        return;
    };
    let mut command = std::process::Command::new(&cli);
    command
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--state-dir")
        .arg(&state_dir.root)
        .args(args)
        .stdin(std::process::Stdio::null());
    picker::hide_console(&mut command);
    match command.spawn() {
        Ok(mut child) => {
            // Reap the child when it exits, so that it leaves no zombie on Unix.
            std::thread::spawn(move || child.wait());
        }
        Err(error) => log::warn!("cannot start {}: {error}", cli.display()),
    }
}

/// Returns the CLI executable: the running one, or the one next to the running agent.
fn cli_executable() -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let name = format!("claude-account-sync{}", std::env::consts::EXE_SUFFIX);
    if current.file_name().is_some_and(|n| n == name.as_str()) {
        return Some(current);
    }
    Some(current.with_file_name(name)).filter(|path| path.is_file())
}

fn read_sign_in(data_dir: &Path, config: &Map<String, Value>) -> Result<SignIn, ProfileError> {
    let mut oauth = Map::new();
    for key in OAUTH_KEYS {
        if let Some(value) = config.get(key) {
            oauth.insert(key.to_owned(), value.clone());
        }
    }
    let cookies_dir = cookies_dir_name(data_dir);
    let folder = data_dir.join(&cookies_dir);
    let mut cookies = Vec::new();
    for name in COOKIE_FILES {
        let path = folder.join(name);
        if let Some(bytes) = read_regular(&path, MAX_FILE_LEN).map_err(io_error(&path))? {
            // An empty journal is what SQLite leaves after a clean close. It carries nothing.
            if name != "Cookies" && bytes.is_empty() {
                continue;
            }
            cookies.push((name.to_owned(), bytes));
        }
    }
    if !cookies.iter().any(|(name, _)| name == "Cookies") {
        return Err(ProfileError::Io {
            path: folder.join("Cookies"),
            source: io::Error::from(io::ErrorKind::NotFound),
        });
    }
    Ok(SignIn {
        oauth,
        cookies_dir,
        cookies,
    })
}

/// Returns where the app keeps its cookie database: `Network` in current versions, the data folder itself in older
/// ones.
fn cookies_dir_name(data_dir: &Path) -> String {
    if !data_dir.join("Network").join("Cookies").is_file() && data_dir.join("Cookies").is_file() {
        String::new()
    } else {
        "Network".to_owned()
    }
}

fn remove_if_exists(path: &Path) -> Result<(), ProfileError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ProfileError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

/// Creates `dir` and, on Unix, makes it readable only by the user. The saved files are encrypted, but they still
/// let anyone who runs as this user sign in.
fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Formats Unix milliseconds as local time with an age, such as `2026-11-18 21:54 (3 h ago)`.
pub fn format_time(millis: i64, now: SystemTime) -> String {
    let Ok(time) = time::OffsetDateTime::from_unix_timestamp_nanos(millis as i128 * 1_000_000)
    else {
        return "unknown".into();
    };
    let offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    let text = time
        .to_offset(offset)
        .format(time::macros::format_description!(
            "[year]-[month]-[day] [hour]:[minute]"
        ))
        .unwrap_or_default();
    let age_secs = (file_ops::unix_millis(now) - millis).max(0) / 1000;
    let age = match age_secs {
        0..60 => "just now".to_owned(),
        60..3600 => format!("{} min ago", age_secs / 60),
        3600..86_400 => format!("{} h ago", age_secs / 3600),
        _ => format!("{} d ago", age_secs / 86_400),
    };
    format!("{text} ({age})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const A: &str = "aaaaaaaa-0000-0000-0000-000000000001";
    const B: &str = "bbbbbbbb-0000-0000-0000-000000000002";

    fn write_app(data_dir: &Path, account: &str, token: &str, cookies: &[u8]) {
        fs::create_dir_all(data_dir.join("Network")).unwrap();
        fs::write(
            data_dir.join("config.json"),
            serde_json::to_vec(&serde_json::json!({
                "locale": "ja-JP",
                "lastKnownAccountUuid": account,
                "oauth:tokenCacheV2": token,
                "oauth:tokenCache": format!("{token}-v1"),
                "userThemeMode": "dark",
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(data_dir.join("Network/Cookies"), cookies).unwrap();
        fs::write(data_dir.join("Network/Cookies-journal"), b"").unwrap();
    }

    fn config(data_dir: &Path) -> Value {
        serde_json::from_slice(&fs::read(data_dir.join("config.json")).unwrap()).unwrap()
    }

    #[test]
    fn reads_login_state() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(LoginState::read(dir.path()), LoginState::default());
        write_app(dir.path(), A, "t", b"c");
        let state = LoginState::read(dir.path());
        assert_eq!(state.signed_in_account(), Some(A));
        fs::write(
            dir.path().join("config.json"),
            format!(r#"{{"lastKnownAccountUuid":"{A}","oauth:tokenCacheV2":""}}"#),
        )
        .unwrap();
        let state = LoginState::read(dir.path());
        assert_eq!(state.account.as_deref(), Some(A));
        assert!(!state.signed_in);
    }

    #[test]
    fn capture_and_restore_switch_accounts() {
        let data = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(state.path());
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);

        write_app(data.path(), A, "token-a", b"cookies-a");
        assert_eq!(
            store.capture(data.path(), A, now).unwrap(),
            CaptureOutcome::Saved
        );
        assert_eq!(
            store.capture(data.path(), A, now).unwrap(),
            CaptureOutcome::Unchanged
        );
        write_app(data.path(), B, "token-b", b"cookies-b");
        store.capture(data.path(), B, now).unwrap();

        // The user changes a setting while B is signed in. Restoring A keeps it.
        let mut current = config(data.path());
        current["userThemeMode"] = "light".into();
        fs::write(
            data.path().join("config.json"),
            serde_json::to_vec(&current).unwrap(),
        )
        .unwrap();

        let backup = Backup::new(&state.path().join("backups"), now);
        store.restore(data.path(), A, &backup).unwrap();
        let restored = config(data.path());
        assert_eq!(restored["lastKnownAccountUuid"], A);
        assert_eq!(restored["oauth:tokenCacheV2"], "token-a");
        assert_eq!(restored["oauth:tokenCache"], "token-a-v1");
        assert_eq!(restored["userThemeMode"], "light");
        assert_eq!(restored["locale"], "ja-JP");
        assert_eq!(
            fs::read(data.path().join("Network/Cookies")).unwrap(),
            b"cookies-a"
        );
        assert!(!data.path().join("Network/Cookies-journal").exists());
        assert_eq!(
            fs::read(backup.root().join("sign-in/Cookies")).unwrap(),
            b"cookies-b"
        );
    }

    #[test]
    fn signed_out_state_is_never_saved_and_sign_out_blocks_restore() {
        let data = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(state.path());
        let t1 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let t2 = t1 + Duration::from_secs(10);

        write_app(data.path(), A, "token-a", b"cookies-a");
        store.capture(data.path(), A, t1).unwrap();
        store.mark_signed_out(A, t2).unwrap();
        assert!(!store.load(A).unwrap().unwrap().can_restore());
        let backup = Backup::new(&state.path().join("backups"), t2);
        assert!(matches!(
            store.restore(data.path(), A, &backup),
            Err(ProfileError::NoSnapshot(_))
        ));

        fs::write(
            data.path().join("config.json"),
            format!(r#"{{"lastKnownAccountUuid":"{A}"}}"#),
        )
        .unwrap();
        assert!(matches!(
            store.capture(data.path(), A, t2),
            Err(ProfileError::NotSignedIn)
        ));

        // Signing in again and saving makes the profile usable again.
        write_app(data.path(), A, "token-a2", b"cookies-a2");
        store
            .capture(data.path(), A, t2 + Duration::from_secs(1))
            .unwrap();
        assert!(store.load(A).unwrap().unwrap().can_restore());
    }

    #[test]
    fn clear_sign_in_keeps_other_settings_and_backs_up() {
        let data = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        write_app(data.path(), A, "token-a", b"cookies-a");
        let backup = Backup::new(&state.path().join("backups"), SystemTime::UNIX_EPOCH);
        ProfileStore::clear_sign_in(data.path(), &backup).unwrap();
        let cleared = config(data.path());
        assert!(cleared.get("oauth:tokenCacheV2").is_none());
        assert!(cleared.get("oauth:tokenCache").is_none());
        assert!(cleared.get("lastKnownAccountUuid").is_none());
        assert_eq!(cleared["userThemeMode"], "dark");
        assert!(!data.path().join("Network/Cookies").exists());
        assert_eq!(
            fs::read(backup.root().join("sign-in/Cookies")).unwrap(),
            b"cookies-a"
        );
        assert_eq!(LoginState::read(data.path()), LoginState::default());
    }

    #[test]
    fn display_name_prefers_label_then_identity() {
        let state = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(state.path());
        store.touch_active(A, SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(
            store.load(A).unwrap().unwrap().display_name(),
            "account aaaaaaaa"
        );
        let identity = Identity {
            display_name: Some("Abelia 2".into()),
            email: Some("a@example.com".into()),
            organization_name: None,
        };
        assert!(store.record_identity(A, &identity).unwrap());
        assert!(!store.record_identity(A, &identity).unwrap());
        let profile = store.load(A).unwrap().unwrap();
        assert_eq!(profile.display_name(), "Abelia 2");
        assert_eq!(profile.email(), Some("a@example.com"));
        store.set_label(A, Some("Work".into())).unwrap();
        assert_eq!(store.load(A).unwrap().unwrap().display_name(), "Work");
    }

    #[test]
    fn resolves_by_uuid_prefix_and_label() {
        let state = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(state.path());
        let now = SystemTime::UNIX_EPOCH;
        store.touch_active(A, now).unwrap();
        store.touch_active(B, now).unwrap();
        store.set_label(B, Some("Work".into())).unwrap();
        assert_eq!(store.resolve(A).unwrap().account, A);
        assert_eq!(store.resolve("aaaa").unwrap().account, A);
        assert_eq!(store.resolve("work").unwrap().account, B);
        assert!(store.resolve("zzzz").is_err());
        assert!(store.resolve("../x").is_err());
        assert!(matches!(
            store.load("../x"),
            Err(ProfileError::InvalidAccount(_))
        ));
    }
}
