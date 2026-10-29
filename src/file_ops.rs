//! File reads and writes that keep the app's folders consistent.
//!
//! Writes go to a temporary file in the destination folder and then replace the destination by rename, so the app
//! never reads a partial file. Temporary names start with `.claude-account-sync-` and end with `.tmp`, and the app
//! ignores them because they match neither `local_*.json` nor any other name it reads.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

/// Reads a regular file of at most `max_len` bytes.
///
/// Returns `Ok(None)` when the file does not exist. Returns an error when the path is a symbolic link or another
/// non-regular file, or when the file is larger than `max_len`. The app refuses links in these folders, and this
/// function refuses them for the same reason.
pub fn read_regular(path: &Path, max_len: u64) -> io::Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a regular file",
        ));
    }
    if metadata.len() > max_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("larger than {max_len} bytes"),
        ));
    }
    fs::read(path).map(Some)
}

/// Writes `bytes` to `path` by replacing it atomically, and creates the parent folder when it is missing.
///
/// When `modified` is `Some`, the new file gets that modification time. Session records keep the source's time so
/// that the app orders copies the same way as the original.
pub fn write_atomic(path: &Path, bytes: &[u8], modified: Option<SystemTime>) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("path has no parent"))?;
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("path has no file name"))?;
    let temporary = parent.join(format!(
        ".claude-account-sync-{}-{}.tmp",
        std::process::id(),
        name.to_string_lossy()
    ));
    let result = (|| {
        let mut file = File::create(&temporary)?;
        file.write_all(bytes)?;
        if let Some(modified) = modified {
            file.set_modified(modified)?;
        }
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// A folder that receives copies of files before this tool replaces or removes them.
///
/// The folder is created on the first save, so a pass that changes nothing leaves no empty folder.
#[derive(Debug)]
pub struct Backup {
    root: PathBuf,
}

impl Backup {
    /// Prepares a backup folder under `backups_dir`, named after `now`.
    pub fn new(backups_dir: &Path, now: SystemTime) -> Self {
        let stamp = time::OffsetDateTime::from(now)
            .format(time::macros::format_description!(
                "[year][month][day]T[hour][minute][second].[subsecond digits:3]Z"
            ))
            .unwrap_or_else(|_| "unknown-time".into());
        Self {
            root: backups_dir.join(stamp),
        }
    }

    /// Returns the folder that this backup writes to.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Saves `bytes` at `relative` inside the backup folder.
    pub fn save(&self, relative: &Path, bytes: &[u8]) -> io::Result<()> {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().unwrap_or(&self.root))?;
        fs::write(path, bytes)
    }
}

/// The subfolder of a backup folder that holds the copy of the whole sessions folder from before the first pass.
pub const INITIAL_BACKUP_DIR: &str = "initial";

/// Removes all but the newest `keep` folders in `backups_dir`.
///
/// Folders that contain [`INITIAL_BACKUP_DIR`] are never removed and do not count toward `keep`, because they
/// are the only copy of the app's data from before this tool changed it.
pub fn prune_backups(backups_dir: &Path, keep: usize) -> io::Result<()> {
    let folders: Vec<PathBuf> = list_backup_folders(backups_dir)?
        .into_iter()
        .filter(|folder| !folder.join(INITIAL_BACKUP_DIR).is_dir())
        .collect();
    let excess = folders.len().saturating_sub(keep);
    for folder in &folders[..excess] {
        fs::remove_dir_all(folder)?;
    }
    Ok(())
}

/// The backup folders and the disk space they use.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupUsage {
    pub dir: PathBuf,
    /// Sum of [`BackupFolder::bytes`].
    pub total_bytes: u64,
    /// Oldest first.
    pub folders: Vec<BackupFolder>,
}

/// One backup folder, named by the UTC time of the pass that wrote it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupFolder {
    pub name: String,
    /// Total size of the regular files in the folder. Symbolic links are not followed.
    pub bytes: u64,
    /// `true` when the folder holds the copy from before the first pass. Pruning keeps such folders.
    pub initial: bool,
}

/// Measures the backup folders in `backups_dir`. Returns an empty list when the folder does not exist.
pub fn backup_usage(backups_dir: &Path) -> io::Result<BackupUsage> {
    let mut folders = Vec::new();
    for path in list_backup_folders(backups_dir)? {
        folders.push(BackupFolder {
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            bytes: dir_size(&path)?,
            initial: path.join(INITIAL_BACKUP_DIR).is_dir(),
        });
    }
    Ok(BackupUsage {
        dir: backups_dir.to_owned(),
        total_bytes: folders.iter().map(|folder| folder.bytes).sum(),
        folders,
    })
}

/// Returns the backup folders in `backups_dir`, oldest first.
///
/// Folder names sort by time because [`Backup::new`] names them with a fixed-width UTC timestamp.
fn list_backup_folders(backups_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut folders: Vec<PathBuf> = match fs::read_dir(backups_dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
            .map(|entry| entry.path())
            .collect(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    folders.sort();
    Ok(folders)
}

fn dir_size(dir: &Path) -> io::Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            total += dir_size(&entry.path())?;
        } else if file_type.is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

/// Returns `time` as milliseconds since the Unix epoch, or 0 for times before the epoch.
pub fn unix_millis(time: SystemTime) -> i64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_content_and_keeps_requested_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("a.json");
        let time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        write_atomic(&path, b"one", None).unwrap();
        write_atomic(&path, b"two", Some(time)).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), time);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn read_regular_reports_missing_and_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        assert!(read_regular(&path, 10).unwrap().is_none());
        fs::write(&path, b"0123456789AB").unwrap();
        assert!(read_regular(&path, 10).is_err());
        assert_eq!(read_regular(&path, 12).unwrap().unwrap().len(), 12);
    }

    #[test]
    fn prune_backups_keeps_newest_and_initial_folders() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "20260101T000000.000Z",
            "20260102T000000.000Z",
            "20260103T000000.000Z",
            "20260104T000000.000Z",
        ] {
            fs::create_dir(dir.path().join(name)).unwrap();
        }
        fs::create_dir(
            dir.path()
                .join("20260101T000000.000Z")
                .join(INITIAL_BACKUP_DIR),
        )
        .unwrap();
        prune_backups(dir.path(), 2).unwrap();
        let left: Vec<String> = backup_usage(dir.path())
            .unwrap()
            .folders
            .into_iter()
            .map(|folder| folder.name)
            .collect();
        assert_eq!(
            left,
            [
                "20260101T000000.000Z",
                "20260103T000000.000Z",
                "20260104T000000.000Z"
            ]
        );
    }

    #[test]
    fn backup_usage_sums_nested_files() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            backup_usage(&dir.path().join("missing"))
                .unwrap()
                .total_bytes,
            0
        );
        let folder = dir.path().join("20260101T000000.000Z");
        fs::create_dir_all(folder.join(INITIAL_BACKUP_DIR).join("a")).unwrap();
        fs::write(
            folder.join(INITIAL_BACKUP_DIR).join("a").join("x.json"),
            b"12345",
        )
        .unwrap();
        fs::write(folder.join("y.json"), b"123").unwrap();
        let usage = backup_usage(dir.path()).unwrap();
        assert_eq!(usage.total_bytes, 8);
        assert!(usage.folders[0].initial);
    }
}
