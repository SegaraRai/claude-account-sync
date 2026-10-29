//! The state that one sync pass leaves for the next pass, and the lock that keeps passes from overlapping.
//!
//! The state stores, for each collection, the hashes that each replica held after the last pass, the winning hash
//! of each key, and the keys that a pass deleted. [`crate::merge`] uses these to tell edits and deletions from
//! entries that a replica has not received yet.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::file_ops;
use crate::merge::{ContentHash, History};

/// Errors from reading or writing the state folder.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("another claude-account-sync process holds {0}")]
    Locked(PathBuf),
    #[error("cannot access {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{path} is not a valid state file: {source}")]
    Invalid {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("{path} has state version {version}, but this build supports version {STATE_VERSION}")]
    UnsupportedVersion { path: PathBuf, version: u32 },
}

const STATE_VERSION: u32 = 1;

/// Deleted keys older than this are dropped from the state. A replica that still holds such a key after this
/// period is treated like a replica that never received the deletion, and its copy is kept.
const DELETED_RETENTION_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// The tool's own folder: the state file, the lock file, backups, and logs.
#[derive(Clone, Debug)]
pub struct StateDir {
    pub root: PathBuf,
}

impl StateDir {
    /// Returns the default folder: `%LOCALAPPDATA%\claude-account-sync` on Windows and
    /// `~/Library/Application Support/claude-account-sync` on macOS.
    pub fn default_root() -> Option<PathBuf> {
        dirs::data_local_dir().map(|dir| dir.join("claude-account-sync"))
    }

    pub fn state_file(&self) -> PathBuf {
        self.root.join("state.json")
    }

    pub fn backups_dir(&self) -> PathBuf {
        self.root.join("backups")
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.json")
    }

    pub fn log_file(&self) -> PathBuf {
        self.root.join("agent.log")
    }

    /// Takes the lock that keeps sync passes from overlapping.
    ///
    /// The lock is released when the returned guard is dropped or the process exits.
    pub fn lock(&self) -> Result<StateLock, StateError> {
        self.lock_file("lock")
    }

    /// Takes the lock that keeps a second watcher from starting while one runs.
    pub fn lock_watcher(&self) -> Result<StateLock, StateError> {
        self.lock_file("watcher.lock")
    }

    fn lock_file(&self, name: &str) -> Result<StateLock, StateError> {
        let path = self.root.join(name);
        let io_error = |source| StateError::Io {
            path: path.clone(),
            source,
        };
        std::fs::create_dir_all(&self.root).map_err(io_error)?;
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io_error)?;
        match file.try_lock() {
            Ok(()) => Ok(StateLock { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(StateError::Locked(path)),
            Err(std::fs::TryLockError::Error(source)) => Err(StateError::Io { path, source }),
        }
    }

    /// Reads the state file. Returns an empty state when the file does not exist.
    pub fn load(&self) -> Result<SyncState, StateError> {
        let path = self.state_file();
        let Some(bytes) =
            file_ops::read_regular(&path, 256 << 20).map_err(|source| StateError::Io {
                path: path.clone(),
                source,
            })?
        else {
            return Ok(SyncState::default());
        };
        let state: SyncState =
            serde_json::from_slice(&bytes).map_err(|source| StateError::Invalid {
                path: path.clone(),
                source,
            })?;
        if state.version != STATE_VERSION {
            return Err(StateError::UnsupportedVersion {
                path,
                version: state.version,
            });
        }
        Ok(state)
    }

    /// Writes `state` to the state file atomically.
    pub fn save(&self, state: &SyncState) -> Result<(), StateError> {
        let path = self.state_file();
        let bytes = serde_json::to_vec_pretty(state).expect("state serializes to JSON");
        file_ops::write_atomic(&path, &bytes, None)
            .map_err(|source| StateError::Io { path, source })
    }
}

/// Holds the lock that [`StateDir::lock`] took.
#[derive(Debug)]
pub struct StateLock {
    _file: File,
}

/// The contents of `state.json`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncState {
    pub version: u32,
    /// Maps a collection name, such as `sessions`, to its state.
    #[serde(default)]
    pub collections: BTreeMap<String, CollectionState>,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            collections: BTreeMap::new(),
        }
    }
}

/// The state of one collection.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionState {
    /// Maps a replica id in `<account>/<org>` form to the hash of each key that the replica held after the last
    /// pass.
    #[serde(default)]
    pub replicas: BTreeMap<String, BTreeMap<String, StoredHash>>,
    /// Maps each key to its winning hash in the last pass.
    #[serde(default)]
    pub winners: BTreeMap<String, StoredHash>,
    /// Maps each deleted key to the time of the deletion, in milliseconds since the Unix epoch.
    #[serde(default)]
    pub deleted: BTreeMap<String, i64>,
}

impl CollectionState {
    /// Returns the base hashes of `replica`, or `None` when the state has no record of it.
    pub fn base_of(&self, replica: &str) -> Option<BTreeMap<String, ContentHash>> {
        self.replicas
            .get(replica)
            .map(|keys| keys.iter().map(|(k, h)| (k.clone(), h.0)).collect())
    }

    /// Returns the deleted keys and previous winners in the form that [`crate::merge::merge`] takes.
    pub fn history(&self) -> History {
        History {
            deleted: self.deleted.keys().cloned().collect::<BTreeSet<_>>(),
            winners: self.winners.iter().map(|(k, h)| (k.clone(), h.0)).collect(),
        }
    }

    /// Drops deleted keys older than the retention period. `now_ms` is milliseconds since the Unix epoch.
    pub fn prune_deleted(&mut self, now_ms: i64) {
        self.deleted
            .retain(|_, deleted_at| now_ms - *deleted_at < DELETED_RETENTION_MS);
    }
}

/// A [`ContentHash`] that serializes as a lowercase hexadecimal string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoredHash(pub ContentHash);

impl Serialize for StoredHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let text: String = self.0.iter().map(|byte| format!("{byte:02x}")).collect();
        serializer.serialize_str(&text)
    }
}

impl<'de> Deserialize<'de> for StoredHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let invalid = || serde::de::Error::custom("expected 64 hexadecimal digits");
        if text.len() != 64 {
            return Err(invalid());
        }
        let mut hash = [0u8; 32];
        for (index, byte) in hash.iter_mut().enumerate() {
            *byte =
                u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).map_err(|_| invalid())?;
        }
        Ok(Self(hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_through_file() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = StateDir {
            root: dir.path().to_owned(),
        };
        assert!(state_dir.load().unwrap().collections.is_empty());
        let mut state = SyncState::default();
        let collection = state.collections.entry("sessions".into()).or_default();
        collection
            .winners
            .insert("local_a".into(), StoredHash([0xab; 32]));
        collection.deleted.insert("local_b".into(), 5);
        state_dir.save(&state).unwrap();
        let loaded = state_dir.load().unwrap();
        assert_eq!(
            loaded.collections["sessions"].winners["local_a"],
            StoredHash([0xab; 32])
        );
        assert_eq!(loaded.collections["sessions"].deleted["local_b"], 5);
    }

    #[test]
    fn second_lock_fails_while_first_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = StateDir {
            root: dir.path().to_owned(),
        };
        let _held = state_dir.lock().unwrap();
        assert!(matches!(state_dir.lock(), Err(StateError::Locked(_))));
    }

    #[test]
    fn prune_deleted_drops_old_keys() {
        let mut collection = CollectionState::default();
        collection.deleted.insert("old".into(), 0);
        collection
            .deleted
            .insert("new".into(), DELETED_RETENTION_MS);
        collection.prune_deleted(DELETED_RETENTION_MS + 1);
        assert_eq!(collection.deleted.keys().collect::<Vec<_>>(), ["new"]);
    }
}
