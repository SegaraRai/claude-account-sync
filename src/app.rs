//! Facts about the installed Claude desktop app: where it stores data, which account it shows, and whether it runs.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The folder under the app's data folder that holds one subfolder per account and organization.
pub const SESSIONS_DIR: &str = "claude-code-sessions";

/// The app's state that decides which replicas this tool may write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStatus {
    /// `true` when any process of the desktop app runs.
    pub running: bool,
    /// The account that the app shows, read from `config.json`. `None` when the file or field is missing.
    pub active_account: Option<String>,
}

impl AppStatus {
    /// Reads the current status of the app whose data folder is `data_dir`.
    pub fn read(data_dir: &Path) -> Self {
        Self {
            running: app_is_running(),
            active_account: read_active_account(data_dir),
        }
    }

    /// Returns `true` when this tool may write files of `account`.
    ///
    /// The running app keeps the active account's records, backlog, and scheduled tasks in memory and writes them
    /// back later. A write to that account while the app runs could be lost or could make the app drop entries.
    /// When the app runs and the active account is unknown, no account is writable.
    pub fn may_write(&self, account: &str) -> bool {
        !self.running
            || self
                .active_account
                .as_deref()
                .is_some_and(|active| active != account)
    }
}

/// Returns the app's data folder.
///
/// On Windows, the app stores data in `%APPDATA%\Claude`. A Microsoft Store installation can redirect that folder
/// to `%LOCALAPPDATA%\Packages\Claude_*\LocalCache\Roaming\Claude`. This function returns the candidate that
/// contains [`SESSIONS_DIR`], and prefers the real `%APPDATA%` folder when both do.
///
/// On macOS, the app stores data in `~/Library/Application Support/Claude`.
pub fn default_data_dir() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if cfg!(windows) {
        if let Some(roaming) = dirs::config_dir() {
            candidates.push(roaming.join("Claude"));
        }
        if let Some(local) = dirs::data_local_dir()
            && let Ok(packages) = std::fs::read_dir(local.join("Packages"))
        {
            for package in packages.filter_map(Result::ok) {
                if package.file_name().to_string_lossy().starts_with("Claude_") {
                    candidates.push(
                        package
                            .path()
                            .join("LocalCache")
                            .join("Roaming")
                            .join("Claude"),
                    );
                }
            }
        }
    } else if let Some(support) = dirs::data_dir() {
        candidates.push(support.join("Claude"));
    }
    candidates
        .iter()
        .find(|dir| dir.join(SESSIONS_DIR).is_dir())
        .or(candidates.first())
        .cloned()
}

/// The fields of the app's `config.json` that this tool reads. Other fields are ignored.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppConfig {
    last_known_account_uuid: Option<String>,
}

fn read_active_account(data_dir: &Path) -> Option<String> {
    let bytes = crate::file_ops::read_regular(&data_dir.join("config.json"), 64 << 20).ok()??;
    serde_json::from_slice::<AppConfig>(&bytes)
        .ok()?
        .last_known_account_uuid
}

/// Returns `true` when a process of the desktop app runs.
///
/// The Claude Code CLI also runs as `claude` or `claude.exe`, so a process name alone is not enough. This function
/// matches the desktop app's executable by its exact file name and by its installation folder.
///
/// Returns `true` when the process list cannot be read, because a wrong `false` would let a pass write the
/// replica that the running app holds in memory.
fn app_is_running() -> bool {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System, UpdateKind};
    let Ok(system) = System::new_with_specifics(
        RefreshKind::nothing()
            .with_processes(ProcessRefreshKind::nothing().with_exe(UpdateKind::Always)),
    ) else {
        return true;
    };
    system
        .processes()
        .values()
        .any(|process| process.exe().is_some_and(is_desktop_app_executable))
}

fn is_desktop_app_executable(path: &Path) -> bool {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        // Microsoft Store installation: `...\WindowsApps\Claude_<version>_<arch>__<publisher>\app\Claude.exe`.
        // Squirrel installation: `%LOCALAPPDATA%\AnthropicClaude\app-<version>\claude.exe`.
        let lower = text.to_lowercase();
        let file_name = lower.rsplit(['\\', '/']).next().unwrap_or_default();
        file_name == "claude.exe"
            && (lower.contains("\\windowsapps\\claude_") || lower.contains("\\anthropicclaude\\"))
    } else {
        text.contains("/Claude.app/Contents/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn may_write_only_inactive_accounts_while_app_runs() {
        let running = AppStatus {
            running: true,
            active_account: Some("a".into()),
        };
        assert!(!running.may_write("a"));
        assert!(running.may_write("b"));
        let unknown = AppStatus {
            running: true,
            active_account: None,
        };
        assert!(!unknown.may_write("b"));
        let stopped = AppStatus {
            running: false,
            active_account: Some("a".into()),
        };
        assert!(stopped.may_write("a"));
    }

    #[test]
    fn reads_active_account_from_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"locale":"ja-JP","lastKnownAccountUuid":"abc"}"#,
        )
        .unwrap();
        assert_eq!(read_active_account(dir.path()).as_deref(), Some("abc"));
    }

    #[cfg(windows)]
    #[test]
    fn recognizes_desktop_app_but_not_cli() {
        assert!(is_desktop_app_executable(Path::new(
            r"C:\Program Files\WindowsApps\Claude_4.29.0.0_x64__pzs8sxrjxfjjc\app\Claude.exe"
        )));
        assert!(is_desktop_app_executable(Path::new(
            r"C:\Users\u\AppData\Local\AnthropicClaude\app-1.0.0\claude.exe"
        )));
        assert!(!is_desktop_app_executable(Path::new(
            r"C:\Users\u\.local\bin\claude.exe"
        )));
    }
}
