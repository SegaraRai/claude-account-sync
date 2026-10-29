//! Discovery of replicas: the `<account>/<org>` folders under the app's sessions folder.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Identifies one replica by the account UUID and organization UUID that name its folders.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ReplicaId {
    pub account: String,
    pub org: String,
}

impl fmt::Display for ReplicaId {
    /// Formats the id as `<account>/<org>`. The state file uses this form as a map key.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.account, self.org)
    }
}

/// One replica folder and whether this pass may write it.
#[derive(Clone, Debug, Serialize)]
pub struct Replica {
    pub id: ReplicaId,
    pub dir: PathBuf,
    pub writable: bool,
}

/// Returns the replicas under `sessions_root`, sorted by id.
///
/// Only folders named by a UUID count as account or organization folders. Returns an empty list when
/// `sessions_root` does not exist.
pub fn discover(sessions_root: &Path) -> io::Result<Vec<ReplicaId>> {
    let mut replicas = Vec::new();
    for account in uuid_dirs(sessions_root)? {
        for org in uuid_dirs(&sessions_root.join(&account))? {
            replicas.push(ReplicaId {
                account: account.clone(),
                org,
            });
        }
    }
    replicas.sort();
    Ok(replicas)
}

fn uuid_dirs(dir: &Path) -> io::Result<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && let Some(name) = entry.file_name().to_str()
            && is_uuid(name)
        {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

/// Returns `true` when `text` is a UUID in the 8-4-4-4-12 hexadecimal form.
pub fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(index, c)| match index {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_uuid_folders_only() {
        let root = tempfile::tempdir().unwrap();
        let account = "9b741bb0-edc4-4cd2-a930-ae810e4829e9";
        let org = "54724713-a025-458a-8bd1-d65e3fe2679d";
        std::fs::create_dir_all(root.path().join(account).join(org)).unwrap();
        std::fs::create_dir_all(root.path().join("skills-plugin").join(org)).unwrap();
        std::fs::create_dir_all(root.path().join(account).join("backlog")).unwrap();
        let found = discover(root.path()).unwrap();
        assert_eq!(
            found,
            vec![ReplicaId {
                account: account.into(),
                org: org.into()
            }]
        );
    }
}
