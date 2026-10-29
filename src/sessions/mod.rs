//! Merges Code tab session records across replicas.
//!
//! Each replica lists its sidebar sessions as `local_<id>.json` files. The app records a deleted session as a
//! marker file `deleted_<id>` in the replica where the user deleted it. The transcripts that records
//! point to live under `~/.claude/projects` and do not depend on the account, so this module copies only the
//! record files.
//!
//! This module does not copy `deleted_<id>` markers to other replicas. The app uses a marker to delete the
//! session's transcript once no replica holds a record for it. Removing the other replicas' records is enough for
//! the app to finish that cleanup, and a copied marker would let a second account delete the transcript again.

mod archive_index;
mod record;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::SystemTime;

use crate::file_ops::{self, read_regular, write_atomic};
use crate::merge::{Action, ContentHash, EntryVersion, OrderKey, ReplicaInput, hash_bytes};
use crate::pass::{Collection, PassContext};
use crate::replica::Replica;

pub use record::{RecordError, SessionRecord};

/// The app refuses to read a record larger than this many bytes.
const MAX_RECORD_LEN: u64 = 8 << 20;

/// The session records collection.
pub struct Sessions;

/// The records of one replica.
pub struct LoadedSessions {
    records: BTreeMap<String, LoadedRecord>,
    tombstones: BTreeSet<String>,
    unreadable: BTreeSet<String>,
    /// `None` when the index file does not exist. `Some(None)` when it exists but is invalid.
    archive_index: Option<Option<BTreeSet<String>>>,
}

struct LoadedRecord {
    bytes: Vec<u8>,
    modified: SystemTime,
    version: EntryVersion,
    is_archived: bool,
}

impl Collection for Sessions {
    const NAME: &'static str = "sessions";
    type Loaded = LoadedSessions;

    fn load(&self, replica: &Replica) -> Result<LoadedSessions, String> {
        let mut loaded = LoadedSessions {
            records: BTreeMap::new(),
            tombstones: BTreeSet::new(),
            unreadable: BTreeSet::new(),
            archive_index: None,
        };
        let entries = std::fs::read_dir(&replica.dir).map_err(|e| e.to_string())?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if let Some(id) = name.strip_prefix(record::TOMBSTONE_PREFIX) {
                if record::is_session_id(id) {
                    loaded.tombstones.insert(id.to_owned());
                }
                continue;
            }
            if name == archive_index::FILE_NAME {
                loaded.archive_index = Some(
                    read_regular(&entry.path(), archive_index::MAX_LEN)
                        .ok()
                        .flatten()
                        .and_then(|bytes| archive_index::parse(&bytes)),
                );
                continue;
            }
            let Some(id) = name
                .strip_suffix(".json")
                .filter(|id| record::is_session_id(id))
            else {
                continue;
            };
            match load_record(&entry.path(), id) {
                Ok(record) => {
                    loaded.records.insert(id.to_owned(), record);
                }
                Err(error) => {
                    log::warn!("{}: cannot use {name}: {error}", replica.id);
                    loaded.unreadable.insert(id.to_owned());
                }
            }
        }
        Ok(loaded)
    }

    fn input(&self, loaded: &LoadedSessions) -> ReplicaInput {
        ReplicaInput {
            current: loaded
                .records
                .iter()
                .map(|(id, record)| (id.clone(), record.version))
                .collect(),
            tombstones: loaded.tombstones.clone(),
            unreadable: loaded.unreadable.clone(),
            ..Default::default()
        }
    }

    fn apply(
        &self,
        context: &mut PassContext<'_>,
        replicas: &[(&Replica, &LoadedSessions)],
        inputs: &[ReplicaInput],
        target: usize,
        actions: &[Action],
    ) -> BTreeMap<String, ContentHash> {
        let (replica, loaded) = replicas[target];
        let base = inputs[target].base.as_ref();
        let mut holdings: BTreeMap<String, ContentHash> = loaded
            .records
            .iter()
            .map(|(id, record)| (id.clone(), record.version.hash))
            .collect();
        let mut archived: BTreeMap<String, bool> = loaded
            .records
            .iter()
            .map(|(id, record)| (id.clone(), record.is_archived))
            .collect();
        let mut changed = false;

        for action in actions {
            let (Action::Copy { key, .. } | Action::Remove { key, .. }) = action;
            let file_name = format!("{key}.json");
            let path = replica.dir.join(&file_name);
            let existing = loaded.records.get(key);

            // The app may have rewritten the file after `load`. Its newer content wins until the next pass.
            let on_disk = read_regular(&path, MAX_RECORD_LEN).ok().flatten();
            if on_disk.as_deref() != existing.map(|record| record.bytes.as_slice()) {
                context.report.problems.push(format!(
                    "sessions: {key} changed in {} during the pass",
                    replica.id
                ));
                continue;
            }
            let has_unsynced_changes = existing
                .is_some_and(|record| base.and_then(|b| b.get(key)) != Some(&record.version.hash));
            let must_back_up = matches!(action, Action::Remove { .. }) || has_unsynced_changes;
            if must_back_up
                && let Some(record) = existing
                && let Err(error) =
                    context.back_up(&replica.id, Path::new(&file_name), &record.bytes)
            {
                context.report.problems.push(format!(
                    "sessions: cannot back up {key} in {}: {error}",
                    replica.id
                ));
                continue;
            }

            let result = match action {
                Action::Copy { source, .. } => {
                    let source_record = &replicas[*source].1.records[key];
                    write_atomic(&path, &source_record.bytes, Some(source_record.modified)).map(
                        |()| {
                            holdings.insert(key.clone(), source_record.version.hash);
                            archived.insert(key.clone(), source_record.is_archived);
                        },
                    )
                }
                Action::Remove { .. } => std::fs::remove_file(&path).map(|()| {
                    holdings.remove(key);
                    archived.remove(key);
                }),
            };
            match result {
                Ok(()) => changed = true,
                Err(error) => context.report.problems.push(format!(
                    "sessions: cannot write {key} in {}: {error}",
                    replica.id
                )),
            }
        }

        if changed {
            update_archive_index(context, replica, loaded, &archived);
        }
        holdings
    }
}

/// Returns the number of session record files in the replica folder `dir`, including records that fail
/// validation. Returns 0 when the folder cannot be read.
pub fn count_records(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_name().to_str().is_some_and(|name| {
                    name.strip_suffix(".json")
                        .is_some_and(record::is_session_id)
                })
            })
            .count()
    })
}

fn load_record(path: &Path, id: &str) -> Result<LoadedRecord, String> {
    let bytes = read_regular(path, MAX_RECORD_LEN)
        .map_err(|e| e.to_string())?
        .ok_or("file disappeared")?;
    let modified = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map_err(|e| e.to_string())?;
    let record = SessionRecord::parse(&bytes, id).map_err(|e| e.to_string())?;
    let version = EntryVersion {
        hash: hash_bytes(&bytes),
        order: OrderKey {
            primary: record.last_activity_at as i64,
            secondary: file_ops::unix_millis(modified),
        },
    };
    Ok(LoadedRecord {
        bytes,
        modified,
        version,
        is_archived: record.is_archived,
    })
}

/// Rewrites the replica's archive index from the records it holds after this pass.
///
/// `archived` maps each readable record to its `isArchived` field. Unreadable records keep the membership that
/// the existing index gives them.
fn update_archive_index(
    context: &mut PassContext<'_>,
    replica: &Replica,
    loaded: &LoadedSessions,
    archived: &BTreeMap<String, bool>,
) {
    let mut ids: BTreeSet<String> = archived
        .iter()
        .filter(|(_, is_archived)| **is_archived)
        .map(|(id, _)| id.clone())
        .collect();
    if let Some(Some(existing)) = &loaded.archive_index {
        ids.extend(
            existing
                .iter()
                .filter(|id| loaded.unreadable.contains(*id))
                .cloned(),
        );
    }
    match &loaded.archive_index {
        None if ids.is_empty() => return,
        Some(Some(existing)) if *existing == ids => return,
        _ => {}
    }
    let path = replica.dir.join(archive_index::FILE_NAME);
    if let Ok(Some(old)) = read_regular(&path, archive_index::MAX_LEN)
        && let Err(error) = context.back_up(&replica.id, Path::new(archive_index::FILE_NAME), &old)
    {
        context.report.problems.push(format!(
            "sessions: cannot back up the archive index of {}: {error}",
            replica.id
        ));
        return;
    }
    if let Err(error) = write_atomic(&path, &archive_index::render(&ids), None) {
        context.report.problems.push(format!(
            "sessions: cannot write the archive index of {}: {error}",
            replica.id
        ));
    }
}
