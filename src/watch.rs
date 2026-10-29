//! Runs sync passes whenever the app changes a replica, and on a fixed interval.
//!
//! The interval catches changes that produce no file event, such as the app starting or stopping, which changes
//! which replicas a pass may write.
//!
//! The watcher reads `config.json` before each pass, and a change to that file starts a pass. A change to
//! `dataDir` takes effect only after the watcher restarts, because the watcher watches the folder that it found
//! at startup.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{EventKind, RecursiveMode, Watcher};

use crate::app::{AppStatus, SESSIONS_DIR};
use crate::config::Config;
use crate::pass::{self, PassOptions};
use crate::state::StateDir;

/// Time without further events before a pass starts. The app writes several files for one change.
const QUIET_PERIOD: Duration = Duration::from_millis(1500);

/// Longest delay between the first event and the pass, even when events keep arriving.
const MAX_DELAY: Duration = Duration::from_secs(10);

/// Watches `data_dir` and runs a pass after each change and at the interval.
///
/// `interval` overrides [`Config::watch_interval_secs`] when it is `Some`. Returns only when the watcher cannot
/// start or stops. Each pass logs its changes and problems. A failed pass, or a config that cannot be read, is
/// logged and retried at the next event or interval.
pub fn run(
    data_dir: &Path,
    state_dir: &StateDir,
    interval: Option<Duration>,
) -> anyhow::Result<()> {
    let _watcher_lock = state_dir.lock_watcher()?;
    let sessions_root = data_dir.join(SESSIONS_DIR);
    let (sender, receiver) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(sender)?;
    watcher.watch(data_dir, RecursiveMode::NonRecursive)?;
    if sessions_root.is_dir() {
        watcher.watch(&sessions_root, RecursiveMode::Recursive)?;
    }
    std::fs::create_dir_all(&state_dir.root)?;
    watcher.watch(&state_dir.root, RecursiveMode::NonRecursive)?;
    log::info!("watching {}", data_dir.display());

    let mut config_interval = Duration::from_secs(Config::default().watch_interval_secs);
    loop {
        match Config::load(&state_dir.config_file()) {
            Ok(config) => {
                config_interval = Duration::from_secs(config.watch_interval_secs);
                run_pass(data_dir, state_dir, &config);
            }
            Err(error) => log::error!("skipped a pass: {error}"),
        }
        let deadline = Instant::now() + interval.unwrap_or(config_interval);
        let mut first_event: Option<Instant> = None;
        loop {
            let now = Instant::now();
            let wait = match first_event {
                Some(first) => QUIET_PERIOD.min((first + MAX_DELAY).saturating_duration_since(now)),
                None => deadline.saturating_duration_since(now),
            };
            match receiver.recv_timeout(wait) {
                Ok(Ok(event)) if is_relevant(&event, data_dir, &sessions_root, state_dir) => {
                    first_event.get_or_insert(now);
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => log::warn!("file watcher error: {error}"),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => anyhow::bail!("file watcher stopped"),
            }
        }
    }
}

fn run_pass(data_dir: &Path, state_dir: &StateDir, config: &Config) {
    match pass::run(
        data_dir,
        state_dir,
        config,
        AppStatus::read(data_dir),
        PassOptions::default(),
    ) {
        Ok(report) => {
            for change in &report.changes {
                log::info!("{change}");
            }
            for problem in &report.problems {
                log::warn!("{problem}");
            }
            if let Some(dir) = &report.backup_dir {
                log::info!("backed up replaced files to {}", dir.display());
            }
        }
        Err(error) => log::error!("pass failed: {error}"),
    }
}

/// Returns `true` for events that can change what a pass does: changes under the sessions folder, except this
/// tool's temporary files, and changes to the app's `config.json`, which names the active account, or to this
/// tool's `config.json`.
fn is_relevant(
    event: &notify::Event,
    data_dir: &Path,
    sessions_root: &Path,
    state_dir: &StateDir,
) -> bool {
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    let app_config = data_dir.join("config.json");
    let tool_config = state_dir.config_file();
    event.paths.iter().any(|path| {
        let ours = path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with(".claude-account-sync-"));
        (path.starts_with(sessions_root) && !ours) || *path == app_config || *path == tool_config
    })
}
