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
pub fn app_is_running() -> bool {
    match app_executables() {
        Some(paths) => !paths.is_empty(),
        None => true,
    }
}

/// Returns the executable paths of the desktop app's running processes, or `None` when the process list cannot be
/// read.
pub fn app_executables() -> Option<Vec<PathBuf>> {
    let system = process_list()?;
    let mut paths: Vec<PathBuf> = system
        .processes()
        .values()
        .filter_map(|process| process.exe())
        .filter(|exe| is_desktop_app_executable(exe))
        .map(Path::to_path_buf)
        .collect();
    paths.sort();
    paths.dedup();
    Some(paths)
}

/// Kills every process of the desktop app without letting it save. Use this only when the app holds no account
/// data in memory, such as after it signed out.
pub fn kill_app() -> anyhow::Result<()> {
    let system = process_list().ok_or_else(|| anyhow::anyhow!("cannot list processes"))?;
    for process in system.processes().values() {
        if process.exe().is_some_and(is_desktop_app_executable) {
            process.kill();
        }
    }
    Ok(())
}

/// Asks the desktop app to quit normally, so that it saves its data first. Returns `false` when the request could
/// not be delivered, and the user has to quit the app.
///
/// On macOS this sends the quit Apple event.
///
/// On Windows, closing the main window only hides the app in the notification area. The main window does quit the
/// app, through the same `app.quit()` as the tray menu's Quit, when Windows ends the session: its `session-end`
/// handler logs "Windows session ending" and quits. This function sends that window the messages that the Restart
/// Manager sends to close an application, `WM_QUERYENDSESSION` and then `WM_ENDSESSION` with
/// `ENDSESSION_CLOSEAPP`. Only the main process's top-level `Chrome_WidgetWin_1` windows receive them, so the
/// helper processes and the rest of the session are not affected.
pub fn request_quit() -> bool {
    if cfg!(target_os = "macos") {
        std::process::Command::new("osascript")
            .args(["-e", "tell application \"Claude\" to quit"])
            .status()
            .is_ok_and(|status| status.success())
    } else {
        #[cfg(windows)]
        {
            win32::request_quit()
        }
        #[cfg(not(windows))]
        false
    }
}

/// Returns `true` when [`request_quit`] can ask the running app to quit: on macOS always, on Windows when the main
/// window is found.
pub fn can_request_quit() -> bool {
    if cfg!(target_os = "macos") {
        return true;
    }
    #[cfg(windows)]
    {
        !win32::main_windows().is_empty()
    }
    #[cfg(not(windows))]
    false
}

/// Returns the process IDs of the app's main processes: app processes whose parent is not an app process. The
/// app's renderer, GPU, and crash handler processes are children of the main process.
#[cfg_attr(not(windows), allow(dead_code))]
fn main_process_ids() -> Vec<u32> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System, UpdateKind};
    let Ok(system) = System::new_with_specifics(
        RefreshKind::nothing()
            .with_processes(ProcessRefreshKind::nothing().with_exe(UpdateKind::Always)),
    ) else {
        return Vec::new();
    };
    let app: std::collections::BTreeSet<sysinfo::Pid> = system
        .processes()
        .iter()
        .filter(|(_, process)| process.exe().is_some_and(is_desktop_app_executable))
        .map(|(pid, _)| *pid)
        .collect();
    app.iter()
        .filter(|pid| {
            system
                .process(**pid)
                .and_then(|process| process.parent())
                .is_none_or(|parent| !app.contains(&parent))
        })
        .map(|pid| pid.as_u32())
        .collect()
}

#[cfg(windows)]
mod win32 {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowThreadProcessId, SMTO_ABORTIFHUNG,
        SendMessageTimeoutW, WM_ENDSESSION, WM_QUERYENDSESSION,
    };

    /// `lParam` of the session messages: the application is being closed, not the whole session.
    const ENDSESSION_CLOSEAPP: LPARAM = 0x1;

    /// How long each message may take before the app counts as hung.
    const SEND_TIMEOUT_MS: u32 = 5000;

    struct Search {
        pids: Vec<u32>,
        found: Vec<HWND>,
    }

    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: `lparam` is the `&mut Search` that `main_windows` passes to `EnumWindows`, which calls this
        // function only while `main_windows` waits.
        let search = unsafe { &mut *(lparam as *mut Search) };
        let mut pid = 0u32;
        // SAFETY: `hwnd` comes from `EnumWindows`, and `pid` is a valid output location.
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if search.pids.contains(&pid) {
            let mut class = [0u16; 64];
            // SAFETY: the buffer holds `class.len()` UTF-16 units.
            let len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
            if String::from_utf16_lossy(&class[..len.max(0) as usize]) == "Chrome_WidgetWin_1" {
                search.found.push(hwnd);
            }
        }
        1
    }

    /// Returns the top-level browser windows of the app's main process, hidden ones included.
    pub fn main_windows() -> Vec<HWND> {
        let mut search = Search {
            pids: super::main_process_ids(),
            found: Vec::new(),
        };
        if search.pids.is_empty() {
            return Vec::new();
        }
        // SAFETY: `visit` matches `WNDENUMPROC` and receives a pointer to `search`, which outlives the call.
        unsafe { EnumWindows(Some(visit), &mut search as *mut Search as LPARAM) };
        search.found
    }

    pub fn request_quit() -> bool {
        let windows = main_windows();
        if windows.is_empty() {
            log::warn!("cannot find the app's main window to ask it to quit");
            return false;
        }
        let send = |hwnd: HWND, message: u32, wparam: usize| -> Option<usize> {
            let mut result = 0usize;
            // SAFETY: `hwnd` is a window handle from `EnumWindows`. A stale handle makes the call fail, which is
            // handled. `result` is a valid output location.
            let sent = unsafe {
                SendMessageTimeoutW(
                    hwnd,
                    message,
                    wparam,
                    ENDSESSION_CLOSEAPP,
                    SMTO_ABORTIFHUNG,
                    SEND_TIMEOUT_MS,
                    &mut result,
                )
            };
            (sent != 0).then_some(result)
        };
        // The app may refuse in WM_QUERYENDSESSION. Then it is not told to end.
        if windows
            .iter()
            .any(|hwnd| send(*hwnd, WM_QUERYENDSESSION, 0) == Some(0))
        {
            log::warn!("the app refused to quit");
            return false;
        }
        let mut delivered = false;
        for hwnd in &windows {
            delivered |= send(*hwnd, WM_ENDSESSION, 1).is_some();
        }
        delivered
    }
}

/// Starts the desktop app.
///
/// `seen` lists executables of the app that ran earlier, from [`app_executables`]. On Windows, a Microsoft Store
/// installation starts through its application user model ID, because its executable cannot be started directly.
pub fn launch_app(seen: &[PathBuf]) -> anyhow::Result<()> {
    use std::process::Command;
    if cfg!(target_os = "macos") {
        let status = Command::new("open").args(["-a", "Claude"]).status()?;
        anyhow::ensure!(status.success(), "`open -a Claude` failed with {status}");
        return Ok(());
    }
    anyhow::ensure!(
        cfg!(windows),
        "launching the app supports only Windows and macOS"
    );
    let family = seen
        .iter()
        .find_map(|exe| store_package_family(exe))
        .or_else(installed_store_package_family);
    let squirrel = dirs::data_local_dir()
        .map(|local| local.join("AnthropicClaude").join("claude.exe"))
        .filter(|path| path.is_file());
    let prefers_squirrel = seen.iter().any(|exe| {
        exe.to_string_lossy()
            .to_lowercase()
            .contains(r"\anthropicclaude\")
    });
    match (family, squirrel) {
        (_, Some(squirrel)) if prefers_squirrel => {
            Command::new(squirrel).spawn()?;
        }
        (Some(family), _) => {
            // explorer.exe exits with code 1 even when it starts the app, so its status is ignored.
            Command::new("explorer.exe")
                .arg(format!(r"shell:AppsFolder\{family}!Claude"))
                .status()?;
        }
        (None, Some(squirrel)) => {
            Command::new(squirrel).spawn()?;
        }
        (None, None) => anyhow::bail!("cannot find the installed Claude desktop app"),
    }
    Ok(())
}

/// Returns the package family name, such as `Claude_pzs8sxrjxfjjc`, of a Microsoft Store executable path like
/// `...\WindowsApps\Claude_6.6395.0.0_x64__pzs8sxrjxfjjc\app\Claude.exe`.
fn store_package_family(exe: &Path) -> Option<String> {
    let folder = exe
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .find(|name| name.starts_with("Claude_") && name.contains("__"))?;
    let publisher = folder.rsplit("__").next()?;
    Some(format!("Claude_{publisher}"))
}

fn installed_store_package_family() -> Option<String> {
    let packages = dirs::data_local_dir()?.join("Packages");
    std::fs::read_dir(packages)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .find(|name| name.starts_with("Claude_"))
}

/// A Claude Code session that runs in the desktop app.
/// How recently a session must have written its transcript to count as running.
///
/// A session writes its transcript as a turn goes on. A long tool call, such as a build, can leave it silent for a
/// few minutes, so the window is generous.
pub const ACTIVE_SESSION_WINDOW: std::time::Duration = std::time::Duration::from_secs(300);

/// A Claude Code session of the desktop app that is probably in the middle of a turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningSession {
    pub cli_session_id: String,
    /// The session's title from its sidebar record, when one is found.
    pub title: Option<String>,
    /// When the session last wrote its transcript, in Unix milliseconds.
    pub last_write_at: i64,
}

/// Lists the desktop app's Claude Code sessions that wrote their transcript in the last
/// [`ACTIVE_SESSION_WINDOW`]. Quitting the app stops them.
///
/// The process list cannot tell which session a CLI process serves: the app starts spare processes ahead of time
/// and hands one to each new session, so the command line names no session. Instead, this function looks for
/// transcripts under `~/.claude/projects/` (or `$CLAUDE_CONFIG_DIR/projects/`) that changed recently, and keeps
/// those of sessions that have a sidebar record, which leaves out sessions of the CLI in a terminal.
pub fn running_sessions(data_dir: &Path) -> Vec<RunningSession> {
    let Some(projects) = claude_config_dir().map(|dir| dir.join("projects")) else {
        return Vec::new();
    };
    let now = std::time::SystemTime::now();
    let mut recent = Vec::new();
    for project in std::fs::read_dir(projects).into_iter().flatten().flatten() {
        for entry in std::fs::read_dir(project.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".jsonl") else {
                continue;
            };
            let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
                continue;
            };
            if now
                .duration_since(modified)
                .is_ok_and(|age| age <= ACTIVE_SESSION_WINDOW)
                || modified > now
            {
                recent.push((id.to_owned(), crate::file_ops::unix_millis(modified)));
            }
        }
    }
    if recent.is_empty() {
        return Vec::new();
    }
    let titles = crate::sessions::titles_by_cli_session(&data_dir.join(SESSIONS_DIR));
    let mut sessions: Vec<RunningSession> = recent
        .into_iter()
        .filter_map(|(id, last_write_at)| {
            Some(RunningSession {
                title: Some(titles.get(&id)?.clone()),
                cli_session_id: id,
                last_write_at,
            })
        })
        .collect();
    sessions.sort_by_key(|session| std::cmp::Reverse(session.last_write_at));
    sessions.dedup_by(|a, b| a.cli_session_id == b.cli_session_id);
    sessions
}

/// Returns the Claude Code CLI's config folder: `$CLAUDE_CONFIG_DIR`, or `~/.claude`.
pub fn claude_config_dir() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => dirs::home_dir().map(|home| home.join(".claude")),
    }
}

fn process_list() -> Option<sysinfo::System> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System, UpdateKind};
    System::new_with_specifics(
        RefreshKind::nothing()
            .with_processes(ProcessRefreshKind::nothing().with_exe(UpdateKind::Always)),
    )
    .ok()
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

    #[test]
    fn finds_store_package_family() {
        assert_eq!(
            store_package_family(Path::new(
                r"C:\Program Files\WindowsApps\Claude_6.6395.0.0_x64__pzs8sxrjxfjjc\app\Claude.exe"
            ))
            .as_deref(),
            if cfg!(windows) {
                Some("Claude_pzs8sxrjxfjjc")
            } else {
                None
            }
        );
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
