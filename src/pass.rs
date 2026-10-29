//! One sync pass: discover replicas, merge every collection, apply the plan, and save the state.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

use crate::app::{AppStatus, SESSIONS_DIR};
use crate::config::Config;
use crate::file_ops::{self, Backup};
use crate::item_file::ItemFormat;
use crate::merge::{self, Action, ContentHash, Outcome, ReplicaInput};
use crate::replica::{self, Replica, ReplicaId};
use crate::state::{StateDir, StateError, StoredHash};
use crate::{backlog, scheduled_tasks, sessions};

/// Errors that stop a pass before it changes any replica.
#[derive(Debug, thiserror::Error)]
pub enum PassError {
    #[error(transparent)]
    State(#[from] StateError),
    #[error("cannot list replicas in {path}: {source}")]
    Discover {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot back up {path} before the first pass: {source}")]
    InitialBackup {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Options for one pass.
#[derive(Clone, Copy, Debug, Default)]
pub struct PassOptions {
    /// Plans and reports changes without writing replicas, backups, or the state.
    pub dry_run: bool,
}

/// What a pass changed or would change, and what it could not read or write.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub status: Option<AppStatus>,
    /// Replicas that the pass merged.
    pub replicas: Vec<Replica>,
    /// Replicas that the config excludes. The pass neither read nor wrote them.
    pub excluded: Vec<ReplicaId>,
    /// Collections that the config disables.
    pub disabled_collections: Vec<&'static str>,
    pub changes: Vec<Change>,
    /// Problems that skipped a replica, an entry, or a write. The pass continues after each of them.
    pub problems: Vec<String>,
    /// The backup folder, when the pass saved at least one file there.
    pub backup_dir: Option<PathBuf>,
}

/// One planned or applied change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub collection: &'static str,
    pub key: String,
    pub target: ReplicaId,
    #[serde(flatten)]
    pub kind: ChangeKind,
}

/// Serializes as `"kind": "copy"` with a `from` field, or as `"kind": "remove"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChangeKind {
    Copy { from: ReplicaId },
    Remove,
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ChangeKind::Copy { from } => {
                write!(
                    f,
                    "{}: copy {} from {} to {}",
                    self.collection, self.key, from, self.target
                )
            }
            ChangeKind::Remove => write!(
                f,
                "{}: remove {} from {}",
                self.collection, self.key, self.target
            ),
        }
    }
}

/// The state that collections share during one pass.
pub struct PassContext<'a> {
    pub backup: &'a Backup,
    pub dry_run: bool,
    pub report: &'a mut Report,
}

impl PassContext<'_> {
    /// Saves `bytes` in the backup folder at `<account>/<org>/<relative>`.
    pub fn back_up(
        &mut self,
        replica: &ReplicaId,
        relative: &Path,
        bytes: &[u8],
    ) -> std::io::Result<()> {
        let path = Path::new(&replica.account)
            .join(&replica.org)
            .join(relative);
        self.backup.save(&path, bytes)?;
        self.report.backup_dir = Some(self.backup.root().to_owned());
        Ok(())
    }
}

/// A kind of data that the pass merges across replicas.
pub trait Collection {
    /// The collection's key in the state file.
    const NAME: &'static str;

    /// The data that [`Collection::load`] reads from one replica.
    type Loaded;

    /// Reads one replica. An error skips this replica for this collection in this pass.
    fn load(&self, replica: &Replica) -> Result<Self::Loaded, String>;

    /// Returns the entries, tombstones, and unreadable keys of a loaded replica. The caller fills in
    /// [`ReplicaInput::writable`] and [`ReplicaInput::base`].
    fn input(&self, loaded: &Self::Loaded) -> ReplicaInput;

    /// Applies `actions`, which all target `loaded[target]`, and returns the hash of each key that the target holds
    /// afterwards.
    ///
    /// `inputs[target]` contains the target's base, which tells whether a replaced entry had changes that no pass
    /// has copied yet. Implementations back up such entries and every removed entry. They skip a write when the
    /// target changed after `load`, and they report each skipped write in `context.report.problems`.
    fn apply(
        &self,
        context: &mut PassContext<'_>,
        replicas: &[(&Replica, &Self::Loaded)],
        inputs: &[ReplicaInput],
        target: usize,
        actions: &[Action],
    ) -> BTreeMap<String, ContentHash>;
}

/// Runs one pass over the app data in `data_dir`.
///
/// `config` selects the replicas and collections. `status` decides which replicas the pass may write. Callers read
/// it with [`AppStatus::read`] right before the pass.
pub fn run(
    data_dir: &Path,
    state_dir: &StateDir,
    config: &Config,
    status: AppStatus,
    options: PassOptions,
) -> Result<Report, PassError> {
    let _lock = state_dir.lock()?;
    let mut state = state_dir.load()?;
    let sessions_root = data_dir.join(SESSIONS_DIR);
    let ids = replica::discover(&sessions_root).map_err(|source| PassError::Discover {
        path: sessions_root.clone(),
        source,
    })?;
    let (excluded, ids): (Vec<ReplicaId>, Vec<ReplicaId>) =
        ids.into_iter().partition(|id| config.is_excluded(id));
    let replicas: Vec<Replica> = ids
        .into_iter()
        .map(|id| Replica {
            dir: sessions_root.join(&id.account).join(&id.org),
            writable: status.may_write(&id.account),
            id,
        })
        .collect();

    let now = SystemTime::now();
    let backup = Backup::new(&state_dir.backups_dir(), now);
    let mut report = Report {
        status: Some(status),
        replicas: replicas.clone(),
        excluded,
        ..Default::default()
    };

    // The first pass copies the whole sessions folder, so the user can restore the app's data as it was before
    // this tool changed anything.
    if state.collections.is_empty() && !options.dry_run && sessions_root.is_dir() {
        copy_tree(
            &sessions_root,
            &backup.root().join(file_ops::INITIAL_BACKUP_DIR),
        )
        .map_err(|source| PassError::InitialBackup {
            path: sessions_root.clone(),
            source,
        })?;
        report.backup_dir = Some(backup.root().to_owned());
    }

    let now_ms = file_ops::unix_millis(now);
    let mut context = PassContext {
        backup: &backup,
        dry_run: options.dry_run,
        report: &mut report,
    };
    let switches = config.collections;
    if switches.sessions {
        run_collection(
            &sessions::Sessions,
            &replicas,
            &mut state,
            &mut context,
            now_ms,
        );
    } else {
        context
            .report
            .disabled_collections
            .push(sessions::Sessions::NAME);
    }
    if switches.backlog {
        run_collection(
            &backlog::BACKLOG,
            &replicas,
            &mut state,
            &mut context,
            now_ms,
        );
    } else {
        context
            .report
            .disabled_collections
            .push(backlog::Backlog::NAME);
    }
    if switches.scheduled_tasks {
        run_collection(
            &scheduled_tasks::SCHEDULED_TASKS,
            &replicas,
            &mut state,
            &mut context,
            now_ms,
        );
    } else {
        context
            .report
            .disabled_collections
            .push(scheduled_tasks::ScheduledTasks::NAME);
    }

    if !options.dry_run {
        state_dir.save(&state)?;
        if report.backup_dir.is_some()
            && let Err(error) =
                file_ops::prune_backups(&state_dir.backups_dir(), config.backups_to_keep)
        {
            report
                .problems
                .push(format!("cannot remove old backups: {error}"));
        }
    }
    Ok(report)
}

fn run_collection<C: Collection>(
    collection: &C,
    replicas: &[Replica],
    state: &mut crate::state::SyncState,
    context: &mut PassContext<'_>,
    now_ms: i64,
) {
    let collection_state = state.collections.entry(C::NAME.to_owned()).or_default();

    let mut loaded = Vec::new();
    for replica in replicas {
        match collection.load(replica) {
            Ok(data) => loaded.push((replica, data)),
            Err(error) => context.report.problems.push(format!(
                "{}: skipped {}: {error}",
                C::NAME,
                replica.id
            )),
        }
    }
    let loaded_refs: Vec<(&Replica, &C::Loaded)> = loaded.iter().map(|(r, d)| (*r, d)).collect();
    let inputs: Vec<ReplicaInput> = loaded_refs
        .iter()
        .map(|(replica, data)| ReplicaInput {
            writable: replica.writable,
            base: collection_state.base_of(&replica.id.to_string()),
            ..collection.input(data)
        })
        .collect();

    let plan = merge::merge(&inputs, &collection_state.history());
    for action in &plan.actions {
        let (key, target, kind) = match action {
            Action::Copy {
                key,
                source,
                target,
            } => (
                key,
                *target,
                ChangeKind::Copy {
                    from: loaded_refs[*source].0.id.clone(),
                },
            ),
            Action::Remove { key, target } => (key, *target, ChangeKind::Remove),
        };
        context.report.changes.push(Change {
            collection: C::NAME,
            key: key.clone(),
            target: loaded_refs[target].0.id.clone(),
            kind,
        });
    }
    if context.dry_run {
        return;
    }

    for (index, (replica, _)) in loaded_refs.iter().enumerate() {
        let actions: Vec<Action> = plan
            .actions
            .iter()
            .filter(|action| match action {
                Action::Copy { target, .. } | Action::Remove { target, .. } => *target == index,
            })
            .cloned()
            .collect();
        let holdings = if actions.is_empty() {
            inputs[index]
                .current
                .iter()
                .map(|(k, v)| (k.clone(), v.hash))
                .collect()
        } else {
            collection.apply(context, &loaded_refs, &inputs, index, &actions)
        };
        collection_state.replicas.insert(
            replica.id.to_string(),
            holdings
                .into_iter()
                .map(|(k, h)| (k, StoredHash(h)))
                .collect(),
        );
    }

    let known: Vec<String> = replicas.iter().map(|r| r.id.to_string()).collect();
    collection_state.replicas.retain(|id, _| known.contains(id));
    for (key, outcome) in &plan.outcomes {
        match outcome {
            Outcome::Keep { version, .. } => {
                collection_state
                    .winners
                    .insert(key.clone(), StoredHash(version.hash));
            }
            Outcome::Delete => {
                collection_state.winners.remove(key);
                collection_state
                    .deleted
                    .entry(key.clone())
                    .or_insert(now_ms);
            }
        }
    }
    collection_state.prune_deleted(now_ms);
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = to.join(entry.file_name());
        if file_type.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
