//! The user's settings: `config.json` in the state folder.
//!
//! The file is optional. A missing file means the defaults, and a missing field means that field's default. An
//! unknown field or an invalid value is an error, so a typo never silently changes what the tool syncs. The CLI
//! changes settings through [`Config::set`] and [`Config::reset`], which apply the same validation.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::file_ops::{read_regular, write_atomic};
use crate::replica::{ReplicaId, is_uuid};

/// Errors from reading, changing, or writing the settings.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not a valid config: {source}")]
    Invalid {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error(
        "{path} has config version {version}, but this build supports version {CONFIG_VERSION}"
    )]
    UnsupportedVersion { path: PathBuf, version: u32 },
    #[error("invalid value {value:?} for {key}: {reason}")]
    InvalidValue {
        key: ConfigKey,
        value: String,
        reason: String,
    },
    #[error(transparent)]
    InvalidPattern(#[from] PatternError),
}

const CONFIG_VERSION: u32 = 1;

/// Allowed range of [`Config::watch_interval_secs`].
const WATCH_INTERVAL_RANGE: std::ops::RangeInclusive<u64> = 5..=86_400;

/// Allowed range of [`Config::backups_to_keep`].
const BACKUPS_TO_KEEP_RANGE: std::ops::RangeInclusive<usize> = 1..=10_000;

/// The contents of `config.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    /// Which collections the tool merges. A disabled collection is neither read nor written, and its state is
    /// kept for when it is enabled again.
    #[serde(default)]
    pub collections: CollectionSwitches,
    /// Replicas that the tool neither reads nor writes. A replica that is included again is merged like a new
    /// replica: its entries are added to the others, and entries that the others deleted meanwhile are removed
    /// from it.
    #[serde(default)]
    pub excluded: BTreeSet<ReplicaPattern>,
    /// Seconds between passes of the watcher when no change arrives. From 5 to 86400.
    #[serde(default = "default_watch_interval")]
    pub watch_interval_secs: u64,
    /// Number of backup folders that a pass keeps. From 1 to 10000.
    #[serde(default = "default_backups_to_keep")]
    pub backups_to_keep: usize,
    /// The app's data folder. `None` means that the tool finds the folder itself. The `--data-dir` option
    /// overrides this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<PathBuf>,
}

fn default_version() -> u32 {
    CONFIG_VERSION
}

fn default_watch_interval() -> u64 {
    60
}

fn default_backups_to_keep() -> usize {
    50
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            collections: CollectionSwitches::default(),
            excluded: BTreeSet::new(),
            watch_interval_secs: default_watch_interval(),
            backups_to_keep: default_backups_to_keep(),
            data_dir: None,
        }
    }
}

/// Switches for each collection. All collections are enabled by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectionSwitches {
    #[serde(default = "enabled")]
    pub sessions: bool,
    #[serde(default = "enabled")]
    pub backlog: bool,
    #[serde(default = "enabled")]
    pub scheduled_tasks: bool,
}

fn enabled() -> bool {
    true
}

impl Default for CollectionSwitches {
    fn default() -> Self {
        Self {
            sessions: true,
            backlog: true,
            scheduled_tasks: true,
        }
    }
}

/// A setting that `config set` and `config reset` change. Each variant maps to the kebab-case name on the
/// command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ConfigKey {
    /// `collections.sessions`: `true` or `false`.
    Sessions,
    /// `collections.backlog`: `true` or `false`.
    Backlog,
    /// `collections.scheduledTasks`: `true` or `false`.
    ScheduledTasks,
    /// `watchIntervalSecs`: an integer from 5 to 86400.
    WatchIntervalSecs,
    /// `backupsToKeep`: an integer from 1 to 10000.
    BackupsToKeep,
    /// `dataDir`: an absolute path.
    DataDir,
}

impl fmt::Display for ConfigKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Sessions => "sessions",
            Self::Backlog => "backlog",
            Self::ScheduledTasks => "scheduled-tasks",
            Self::WatchIntervalSecs => "watch-interval-secs",
            Self::BackupsToKeep => "backups-to-keep",
            Self::DataDir => "data-dir",
        };
        f.write_str(name)
    }
}

impl Config {
    /// Reads the config at `path`. Returns the defaults when the file does not exist.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let io_error = |source| ConfigError::Io {
            path: path.to_owned(),
            source,
        };
        let Some(bytes) = read_regular(path, 1 << 20).map_err(io_error)? else {
            return Ok(Self::default());
        };
        let config: Self =
            serde_json::from_slice(&bytes).map_err(|source| ConfigError::Invalid {
                path: path.to_owned(),
                source,
            })?;
        if config.version != CONFIG_VERSION {
            return Err(ConfigError::UnsupportedVersion {
                path: path.to_owned(),
                version: config.version,
            });
        }
        config.validate().map_err(|error| match error {
            ConfigError::InvalidValue { key, value, reason } => ConfigError::InvalidValue {
                key,
                value,
                reason: format!("{reason} (in {})", path.display()),
            },
            other => other,
        })?;
        Ok(config)
    }

    /// Writes the config to `path` atomically, as JSON indented by two spaces.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let mut bytes = serde_json::to_vec_pretty(self).expect("config serializes to JSON");
        bytes.push(b'\n');
        write_atomic(path, &bytes, None).map_err(|source| ConfigError::Io {
            path: path.to_owned(),
            source,
        })
    }

    /// Sets `key` to `value`, which uses the format that [`ConfigKey`] describes. Leaves the config unchanged on
    /// error.
    pub fn set(&mut self, key: ConfigKey, value: &str) -> Result<(), ConfigError> {
        let invalid = |reason: &str| ConfigError::InvalidValue {
            key,
            value: value.to_owned(),
            reason: reason.to_owned(),
        };
        let parse_bool = || match value {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(invalid("expected true or false")),
        };
        let mut next = self.clone();
        match key {
            ConfigKey::Sessions => next.collections.sessions = parse_bool()?,
            ConfigKey::Backlog => next.collections.backlog = parse_bool()?,
            ConfigKey::ScheduledTasks => next.collections.scheduled_tasks = parse_bool()?,
            ConfigKey::WatchIntervalSecs => {
                next.watch_interval_secs =
                    value.parse().map_err(|_| invalid("expected an integer"))?
            }
            ConfigKey::BackupsToKeep => {
                next.backups_to_keep = value.parse().map_err(|_| invalid("expected an integer"))?
            }
            ConfigKey::DataDir => next.data_dir = Some(PathBuf::from(value)),
        }
        next.validate()?;
        *self = next;
        Ok(())
    }

    /// Sets `key` back to its default.
    pub fn reset(&mut self, key: ConfigKey) {
        let defaults = Self::default();
        match key {
            ConfigKey::Sessions => self.collections.sessions = defaults.collections.sessions,
            ConfigKey::Backlog => self.collections.backlog = defaults.collections.backlog,
            ConfigKey::ScheduledTasks => {
                self.collections.scheduled_tasks = defaults.collections.scheduled_tasks
            }
            ConfigKey::WatchIntervalSecs => self.watch_interval_secs = defaults.watch_interval_secs,
            ConfigKey::BackupsToKeep => self.backups_to_keep = defaults.backups_to_keep,
            ConfigKey::DataDir => self.data_dir = None,
        }
    }

    /// Returns [`Config::data_dir`], or the folder that [`crate::app::default_data_dir`] finds when the field is
    /// not set.
    pub fn resolve_data_dir(&self) -> Option<PathBuf> {
        self.data_dir.clone().or_else(crate::app::default_data_dir)
    }

    /// Returns `true` when `id` matches an entry of [`Config::excluded`].
    pub fn is_excluded(&self, id: &ReplicaId) -> bool {
        self.excluded.iter().any(|pattern| pattern.matches(id))
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |key, value: String, reason: &str| ConfigError::InvalidValue {
            key,
            value,
            reason: reason.to_owned(),
        };
        if !WATCH_INTERVAL_RANGE.contains(&self.watch_interval_secs) {
            return Err(invalid(
                ConfigKey::WatchIntervalSecs,
                self.watch_interval_secs.to_string(),
                "expected 5 to 86400",
            ));
        }
        if !BACKUPS_TO_KEEP_RANGE.contains(&self.backups_to_keep) {
            return Err(invalid(
                ConfigKey::BackupsToKeep,
                self.backups_to_keep.to_string(),
                "expected 1 to 10000",
            ));
        }
        if let Some(dir) = &self.data_dir
            && !dir.is_absolute()
        {
            return Err(invalid(
                ConfigKey::DataDir,
                dir.display().to_string(),
                "expected an absolute path",
            ));
        }
        Ok(())
    }
}

/// An error in a [`ReplicaPattern`].
#[derive(Debug, thiserror::Error)]
#[error("invalid replica pattern {0:?}: expected <account UUID> or <account UUID>/<org UUID>")]
pub struct PatternError(String);

/// Selects the replicas of one account (`<account>`) or one replica (`<account>/<org>`). UUIDs are stored in
/// lowercase, which is how the app names its folders.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ReplicaPattern {
    account: String,
    org: Option<String>,
}

impl ReplicaPattern {
    pub fn matches(&self, id: &ReplicaId) -> bool {
        self.account == id.account && self.org.as_ref().is_none_or(|org| *org == id.org)
    }
}

impl FromStr for ReplicaPattern {
    type Err = PatternError;

    fn from_str(text: &str) -> Result<Self, PatternError> {
        let lower = text.trim().to_ascii_lowercase();
        let (account, org) = match lower.split_once('/') {
            Some((account, org)) => (account, Some(org)),
            None => (lower.as_str(), None),
        };
        if !is_uuid(account) || org.is_some_and(|org| !is_uuid(org)) {
            return Err(PatternError(text.to_owned()));
        }
        Ok(Self {
            account: account.to_owned(),
            org: org.map(str::to_owned),
        })
    }
}

impl TryFrom<String> for ReplicaPattern {
    type Error = PatternError;

    fn try_from(text: String) -> Result<Self, PatternError> {
        text.parse()
    }
}

impl From<ReplicaPattern> for String {
    fn from(pattern: ReplicaPattern) -> String {
        pattern.to_string()
    }
}

impl fmt::Display for ReplicaPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.org {
            Some(org) => write!(f, "{}/{org}", self.account),
            None => f.write_str(&self.account),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT: &str = "9b741bb0-edc4-4cd2-a930-ae810e4829e9";
    const ORG: &str = "54724713-a025-458a-8bd1-d65e3fe2679d";

    #[test]
    fn missing_file_and_fields_use_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        assert_eq!(Config::load(&path).unwrap(), Config::default());
        std::fs::write(&path, r#"{"collections":{"backlog":false}}"#).unwrap();
        let config = Config::load(&path).unwrap();
        assert!(!config.collections.backlog);
        assert!(config.collections.sessions);
        assert_eq!(config.watch_interval_secs, 60);
    }

    #[test]
    fn rejects_unknown_fields_and_out_of_range_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"colections":{}}"#).unwrap();
        assert!(matches!(
            Config::load(&path),
            Err(ConfigError::Invalid { .. })
        ));
        std::fs::write(&path, r#"{"watchIntervalSecs":1}"#).unwrap();
        assert!(matches!(
            Config::load(&path),
            Err(ConfigError::InvalidValue { .. })
        ));
        std::fs::write(&path, r#"{"excluded":["me"]}"#).unwrap();
        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn set_validates_and_leaves_config_unchanged_on_error() {
        let mut config = Config::default();
        config.set(ConfigKey::ScheduledTasks, "false").unwrap();
        assert!(!config.collections.scheduled_tasks);
        assert!(config.set(ConfigKey::WatchIntervalSecs, "2").is_err());
        assert!(config.set(ConfigKey::Sessions, "yes").is_err());
        assert!(config.set(ConfigKey::DataDir, "relative").is_err());
        assert_eq!(config.watch_interval_secs, 60);
        config.reset(ConfigKey::ScheduledTasks);
        assert_eq!(config, Config::default());
    }

    #[test]
    fn round_trips_through_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = Config::default();
        config
            .excluded
            .insert(format!("{ACCOUNT}/{ORG}").parse().unwrap());
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
    }

    #[test]
    fn patterns_match_account_or_single_replica() {
        let id = ReplicaId {
            account: ACCOUNT.into(),
            org: ORG.into(),
        };
        let other = ReplicaId {
            account: ACCOUNT.into(),
            org: "00000000-0000-0000-0000-000000000000".into(),
        };
        let account: ReplicaPattern = ACCOUNT.to_uppercase().parse().unwrap();
        let replica: ReplicaPattern = format!("{ACCOUNT}/{ORG}").parse().unwrap();
        assert!(account.matches(&id) && account.matches(&other));
        assert!(replica.matches(&id) && !replica.matches(&other));
        assert!("x/y".parse::<ReplicaPattern>().is_err());
    }
}
