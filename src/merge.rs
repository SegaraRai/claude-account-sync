//! Three-way merge of keyed entries across replicas.
//!
//! A replica is one `<account>/<org>` folder of the Claude desktop app. Each collection (session records, backlog
//! items, scheduled tasks) gives this module one map of entries per replica, and the hashes that the previous
//! sync pass left in that replica. The merge decides, for each key, which version every replica should hold, or
//! whether every replica should drop the key.
//!
//! The module only decides. Callers read and write files and record the new base hashes.

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

/// SHA-256 digest of one entry's serialized content.
pub type ContentHash = [u8; 32];

/// Returns the SHA-256 digest of `bytes`.
pub fn hash_bytes(bytes: &[u8]) -> ContentHash {
    Sha256::digest(bytes).into()
}

/// Ranks competing versions of one entry when more than one replica changed it. The larger key wins.
///
/// The fields are compared in order. Callers put the app's own activity timestamp in `primary` and the file
/// modification time or another tie breaker in `secondary`. Both are milliseconds since the Unix epoch when they
/// are timestamps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct OrderKey {
    pub primary: i64,
    pub secondary: i64,
}

/// One version of an entry in one replica.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryVersion {
    pub hash: ContentHash,
    pub order: OrderKey,
}

/// The entries that one replica holds for one collection.
#[derive(Debug, Default)]
pub struct ReplicaInput {
    /// `false` when the app may hold this replica in memory. The merge then reads the replica but plans no
    /// writes to it.
    pub writable: bool,
    /// Entries that parsed and validated.
    pub current: BTreeMap<String, EntryVersion>,
    /// Keys whose deletion the app recorded in this replica.
    pub tombstones: BTreeSet<String>,
    /// Keys that exist in this replica but could not be read or validated. The merge never plans a write or
    /// removal for these keys in this replica, and it does not treat them as missing.
    pub unreadable: BTreeSet<String>,
    /// Hashes that this replica held after the previous pass. `None` means that the state has no record of this
    /// replica, so a missing key does not mean that the user deleted it.
    pub base: Option<BTreeMap<String, ContentHash>>,
}

/// The decision for one key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every replica should hold the version that replica `source` holds.
    Keep {
        source: usize,
        version: EntryVersion,
    },
    /// Every replica should drop the key.
    Delete,
}

/// One change that the caller should apply to one replica.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Copy the entry `key` from replica `source` into replica `target`.
    Copy {
        key: String,
        source: usize,
        target: usize,
    },
    /// Remove the entry `key` from replica `target`.
    Remove { key: String, target: usize },
}

/// The result of merging one collection.
#[derive(Debug, Default)]
pub struct Plan {
    pub outcomes: BTreeMap<String, Outcome>,
    pub actions: Vec<Action>,
}

/// Keys and versions that earlier passes decided.
#[derive(Debug, Default)]
pub struct History {
    /// Keys that an earlier pass deleted. A key in this set stays deleted even if a replica that could not be
    /// written at that time still holds it.
    pub deleted: BTreeSet<String>,
    /// The winning hash of each key in the previous pass. A replica that could not be written may still hold an
    /// older version that equals its base. This map keeps the earlier decision so that the older version does not
    /// win in a later pass.
    pub winners: BTreeMap<String, ContentHash>,
}

/// Merges one collection across replicas.
///
/// For each key, the merge applies these rules in order:
///
/// 1. The key is deleted when it is in [`History::deleted`], when any replica has a tombstone for it, or when a
///    replica held it in its base and no longer holds it. Deletion wins over concurrent edits because the app
///    rewrites active entries often, and an edit would otherwise bring back an entry that the user deleted.
/// 2. Otherwise, the versions that differ from their replica's base are candidates, and the candidate with the
///    largest [`OrderKey`] wins.
/// 3. When no version differs from its base, a present version that equals the previous winner wins. Without one,
///    the present version with the largest [`OrderKey`] wins.
///
/// The larger hash breaks [`OrderKey`] ties so that every pass picks the same winner.
pub fn merge(replicas: &[ReplicaInput], history: &History) -> Plan {
    let mut keys = BTreeSet::new();
    for replica in replicas {
        keys.extend(replica.current.keys().cloned());
        keys.extend(replica.tombstones.iter().cloned());
        if let Some(base) = &replica.base {
            keys.extend(base.keys().cloned());
        }
    }

    let mut plan = Plan::default();
    for key in keys {
        let Some(outcome) = decide(replicas, history, &key) else {
            continue;
        };
        for (target, replica) in replicas.iter().enumerate() {
            if !replica.writable || replica.unreadable.contains(&key) {
                continue;
            }
            let held = replica.current.get(&key);
            match outcome {
                Outcome::Keep { source, version } => {
                    if held.map(|v| v.hash) != Some(version.hash) {
                        plan.actions.push(Action::Copy {
                            key: key.clone(),
                            source,
                            target,
                        });
                    }
                }
                Outcome::Delete => {
                    if held.is_some() {
                        plan.actions.push(Action::Remove {
                            key: key.clone(),
                            target,
                        });
                    }
                }
            }
        }
        plan.outcomes.insert(key, outcome);
    }
    plan
}

fn decide(replicas: &[ReplicaInput], history: &History, key: &str) -> Option<Outcome> {
    let removed_since_base = |replica: &ReplicaInput| {
        replica
            .base
            .as_ref()
            .is_some_and(|base| base.contains_key(key))
            && !replica.current.contains_key(key)
            && !replica.unreadable.contains(key)
    };
    if history.deleted.contains(key)
        || replicas
            .iter()
            .any(|r| r.tombstones.contains(key) || removed_since_base(r))
    {
        return Some(Outcome::Delete);
    }

    let present: Vec<(usize, EntryVersion)> = replicas
        .iter()
        .enumerate()
        .filter_map(|(index, replica)| replica.current.get(key).map(|version| (index, *version)))
        .collect();
    let changed: Vec<(usize, EntryVersion)> = present
        .iter()
        .copied()
        .filter(|(index, version)| {
            let base_hash = replicas[*index]
                .base
                .as_ref()
                .and_then(|base| base.get(key));
            base_hash != Some(&version.hash)
        })
        .collect();
    if changed.is_empty()
        && let Some(previous) = history.winners.get(key)
        && let Some(&(source, version)) = present.iter().find(|(_, v)| &v.hash == previous)
    {
        return Some(Outcome::Keep { source, version });
    }
    let candidates = if changed.is_empty() {
        &present
    } else {
        &changed
    };
    candidates
        .iter()
        .max_by(|(_, a), (_, b)| a.order.cmp(&b.order).then_with(|| a.hash.cmp(&b.hash)))
        .map(|&(source, version)| Outcome::Keep { source, version })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(byte: u8, primary: i64) -> EntryVersion {
        EntryVersion {
            hash: [byte; 32],
            order: OrderKey {
                primary,
                secondary: 0,
            },
        }
    }

    fn replica(entries: &[(&str, EntryVersion)], base: Option<&[(&str, u8)]>) -> ReplicaInput {
        ReplicaInput {
            writable: true,
            current: entries.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            base: base.map(|b| b.iter().map(|(k, h)| (k.to_string(), [*h; 32])).collect()),
            ..Default::default()
        }
    }

    #[test]
    fn copies_new_entries_to_replicas_without_them() {
        let replicas = [replica(&[("a", version(1, 10))], None), replica(&[], None)];
        let plan = merge(&replicas, &History::default());
        assert_eq!(
            plan.actions,
            vec![Action::Copy {
                key: "a".into(),
                source: 0,
                target: 1
            }]
        );
    }

    #[test]
    fn missing_key_in_new_replica_is_not_a_deletion() {
        let replicas = [
            replica(&[("a", version(1, 10))], Some(&[("a", 1)])),
            replica(&[], None),
        ];
        let plan = merge(&replicas, &History::default());
        assert_eq!(
            plan.actions,
            vec![Action::Copy {
                key: "a".into(),
                source: 0,
                target: 1
            }]
        );
    }

    #[test]
    fn changed_version_wins_over_newer_unchanged_version() {
        let replicas = [
            replica(&[("a", version(2, 5))], Some(&[("a", 1)])),
            replica(&[("a", version(3, 99))], Some(&[("a", 3)])),
        ];
        let plan = merge(&replicas, &History::default());
        assert_eq!(
            plan.actions,
            vec![Action::Copy {
                key: "a".into(),
                source: 0,
                target: 1
            }]
        );
    }

    #[test]
    fn newest_version_wins_when_both_changed() {
        let replicas = [
            replica(&[("a", version(2, 5))], Some(&[("a", 1)])),
            replica(&[("a", version(3, 9))], Some(&[("a", 1)])),
        ];
        let plan = merge(&replicas, &History::default());
        assert_eq!(
            plan.actions,
            vec![Action::Copy {
                key: "a".into(),
                source: 1,
                target: 0
            }]
        );
    }

    #[test]
    fn previous_winner_beats_older_version_left_in_read_only_replica() {
        // The previous pass chose hash 2, but replica 0 was read-only and kept hash 1 with a newer order key.
        let replicas = [
            replica(&[("a", version(1, 99))], Some(&[("a", 1)])),
            replica(&[("a", version(2, 5))], Some(&[("a", 2)])),
        ];
        let history = History {
            winners: [("a".to_string(), [2; 32])].into(),
            ..Default::default()
        };
        let plan = merge(&replicas, &history);
        assert_eq!(
            plan.actions,
            vec![Action::Copy {
                key: "a".into(),
                source: 1,
                target: 0
            }]
        );
    }

    #[test]
    fn removal_since_base_deletes_everywhere() {
        let replicas = [
            replica(&[], Some(&[("a", 1)])),
            replica(&[("a", version(2, 50))], Some(&[("a", 1)])),
        ];
        let plan = merge(&replicas, &History::default());
        assert_eq!(plan.outcomes["a"], Outcome::Delete);
        assert_eq!(
            plan.actions,
            vec![Action::Remove {
                key: "a".into(),
                target: 1
            }]
        );
    }

    #[test]
    fn tombstone_deletes_on_first_pass() {
        let mut first = replica(&[], None);
        first.tombstones.insert("a".into());
        let replicas = [first, replica(&[("a", version(1, 1))], None)];
        let plan = merge(&replicas, &History::default());
        assert_eq!(
            plan.actions,
            vec![Action::Remove {
                key: "a".into(),
                target: 1
            }]
        );
    }

    #[test]
    fn read_only_and_unreadable_replicas_receive_no_actions() {
        let mut read_only = replica(&[], None);
        read_only.writable = false;
        let mut unreadable = replica(&[], None);
        unreadable.unreadable.insert("a".into());
        let replicas = [
            replica(&[("a", version(1, 1))], None),
            read_only,
            unreadable,
        ];
        let plan = merge(&replicas, &History::default());
        assert!(plan.actions.is_empty());
    }

    #[test]
    fn unreadable_entry_is_not_a_removal() {
        let mut broken = replica(&[], Some(&[("a", 1)]));
        broken.unreadable.insert("a".into());
        let replicas = [broken, replica(&[("a", version(1, 1))], Some(&[("a", 1)]))];
        let plan = merge(&replicas, &History::default());
        assert!(matches!(
            plan.outcomes["a"],
            Outcome::Keep { source: 1, .. }
        ));
        assert!(plan.actions.is_empty());
    }

    #[test]
    fn converged_replicas_need_no_actions() {
        let replicas = [
            replica(&[("a", version(1, 1))], Some(&[("a", 1)])),
            replica(&[("a", version(1, 1))], None),
        ];
        assert!(merge(&replicas, &History::default()).actions.is_empty());
    }
}
