//! Switches the desktop app to another saved account without signing out.
//!
//! 1. If Claude Code sessions run in the app, asks the user to confirm, because quitting the app stops them.
//! 2. Stops the app. A signed-out app holds no account data, so it is killed. A signed-in app must save its data
//!    first, so this asks it to quit normally with [`app::request_quit`]. When the request cannot be delivered or
//!    the app is still running after [`AUTO_QUIT_WAIT`], a notice asks the user to quit it from the notification
//!    area or the menu bar, and the switch waits.
//! 3. Saves the current sign-in into its profile, so that switching back later restores it.
//! 4. Restores the target's sign-in, after a backup of the replaced files. For [`Target::NewSignIn`], removes the
//!    sign-in from the app instead, so that the app shows its sign-in page.
//! 5. Starts the app.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, bail};
use serde::Serialize;

use super::{CaptureOutcome, LoginState, ProfileError, ProfileStore};
use crate::app::{self, RunningSession};
use crate::file_ops::Backup;
use crate::state::StateDir;

/// How long the app may take to quit after [`app::request_quit`] before the user is asked to quit it.
const AUTO_QUIT_WAIT: Duration = Duration::from_secs(60);

/// How long to wait for files to unlock after the app's processes exit.
const UNLOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// The user interaction that a switch needs.
pub trait SwitchUi {
    /// Asks whether to stop `sessions` by quitting the app. Returns `false` to cancel the switch.
    fn confirm_stop_sessions(&self, sessions: &[RunningSession]) -> anyhow::Result<bool>;
    /// Tells the user to quit the app when it did not quit on request. `target` names where the switch goes. The
    /// returned notice stays visible until it is dropped.
    fn ask_to_quit(&self, target: &str) -> anyhow::Result<Box<dyn WaitNotice>>;
}

/// A message that stays visible while the switch waits for the app to quit.
pub trait WaitNotice {
    /// Returns `true` when the user cancelled the switch.
    fn cancelled(&mut self) -> bool;
}

/// A notice that only logs, for callers without a window.
pub struct NoNotice;

impl WaitNotice for NoNotice {
    fn cancelled(&mut self) -> bool {
        false
    }
}

#[derive(Clone, Debug)]
pub struct SwitchOptions {
    /// How long to wait for the app to quit.
    pub quit_timeout: Duration,
    /// Starts the app after the switch.
    pub launch: bool,
    /// Skips the confirmation for running sessions.
    pub yes: bool,
    /// Asks the questions but stops before the app is stopped or any file changes.
    pub dry_run: bool,
}

impl Default for SwitchOptions {
    fn default() -> Self {
        Self {
            quit_timeout: Duration::from_secs(600),
            launch: true,
            yes: false,
            dry_run: false,
        }
    }
}

/// Where a switch goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// The saved sign-in of this account.
    Account(String),
    /// The app's sign-in page, so that another account can sign in. The current sign-in is saved and removed
    /// from the app, but not signed out on the server, so it stays usable.
    NewSignIn,
}

/// What [`switch`] did.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchReport {
    pub from: Option<String>,
    /// The account switched to, or `None` for a new sign-in.
    pub to: Option<String>,
    /// `true` when the target was already signed in and nothing changed.
    pub already_active: bool,
    /// `true` when the current sign-in was saved before the switch.
    pub saved_current: bool,
    /// The folder that holds the replaced files.
    pub backup_dir: Option<PathBuf>,
    pub launched: bool,
    pub dry_run: bool,
    /// How the app is stopped: `kill` when it is signed out, `quit` when this tool asks it to quit, `user` when
    /// the user has to quit it, and `None` when it is not running.
    pub stop: Option<&'static str>,
    /// The Claude Code sessions that quitting the app stops.
    pub sessions_to_stop: Vec<RunningSession>,
}

/// Switches the app in `data_dir` to `target`. Returns an error, and leaves the app's files unchanged, when the
/// user cancels or the app does not quit in time.
pub fn switch(
    data_dir: &std::path::Path,
    state_dir: &StateDir,
    target: &Target,
    options: &SwitchOptions,
    ui: &dyn SwitchUi,
) -> anyhow::Result<SwitchReport> {
    let _switch_lock = state_dir
        .lock_named("switch.lock")
        .context("another switch is in progress")?;
    let store = ProfileStore::new(&state_dir.root);
    let before = LoginState::read(data_dir);
    let (account, target_name) = match target {
        Target::Account(account) => {
            let profile = store
                .load(account)?
                .ok_or_else(|| ProfileError::NoSnapshot(account.clone()))?;
            if !profile.can_restore() {
                bail!(
                    "the saved sign-in of {} ended when the app signed out of it; sign in to it once in the app",
                    profile.display_name()
                );
            }
            (Some(account.as_str()), profile.display_name())
        }
        Target::NewSignIn => {
            if !before.signed_in {
                bail!("the app is already signed out; sign in to the other account in the app");
            }
            (None, "a new sign-in".to_owned())
        }
    };

    let seen = app::app_executables().unwrap_or_default();
    let running = !seen.is_empty() || app::app_is_running();
    let mut report = SwitchReport {
        from: before.signed_in_account().map(str::to_owned),
        to: account.map(str::to_owned),
        already_active: false,
        saved_current: false,
        backup_dir: None,
        launched: false,
        dry_run: options.dry_run,
        stop: None,
        sessions_to_stop: Vec::new(),
    };
    if account.is_some() && before.signed_in_account() == account {
        report.already_active = true;
        if !running && options.launch {
            app::launch_app(&seen)?;
            report.launched = true;
        }
        return Ok(report);
    }

    if running {
        report.sessions_to_stop = app::running_sessions(data_dir);
        report.stop = Some(if !before.signed_in {
            "kill"
        } else if app::can_request_quit() {
            "quit"
        } else {
            "user"
        });
        if !report.sessions_to_stop.is_empty()
            && !options.yes
            && !ui.confirm_stop_sessions(&report.sessions_to_stop)?
        {
            bail!("cancelled");
        }
        if options.dry_run {
            return Ok(report);
        }
        stop_app(&before, &target_name, options, ui)?;
    } else if options.dry_run {
        return Ok(report);
    }

    // The sync pass reads the app's config.json, so it waits while this writes it.
    let _sync_lock = wait_for(UNLOCK_TIMEOUT, || state_dir.lock().ok())
        .context("a sync pass holds the state folder")?;
    let now = SystemTime::now();
    let current = LoginState::read(data_dir);
    if let Some(account) = current.signed_in_account() {
        match store.capture(data_dir, account, now) {
            Ok(CaptureOutcome::Saved | CaptureOutcome::Unchanged) => report.saved_current = true,
            Err(error) => {
                return Err(error).context(format!(
                    "cannot save the sign-in of {account}; nothing was switched"
                ));
            }
        }
    } else if let Some(account) = &current.account {
        store.mark_signed_out(account, now)?;
    }
    if account.is_none() && !report.saved_current {
        bail!("the app signed out before its sign-in was saved; nothing was changed");
    }

    let backup = Backup::new(&state_dir.backups_dir(), now);
    // The app's file locks can outlive its processes for a moment on Windows.
    let mut attempt = 0;
    loop {
        let result = match account {
            Some(account) => store.restore(data_dir, account, &backup),
            None => ProfileStore::clear_sign_in(data_dir, &backup),
        };
        match result {
            Ok(()) => break,
            Err(ProfileError::Io { .. }) if attempt < 10 => {
                attempt += 1;
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(error) => return Err(error.into()),
        }
    }
    report.backup_dir = Some(backup.root().to_owned());
    if let Some(account) = account {
        store.touch_active(account, now)?;
    }
    drop(_sync_lock);
    log::info!(
        "switched from {} to {}; backup: {}",
        report.from.as_deref().unwrap_or("(signed out)"),
        account.unwrap_or("a new sign-in"),
        backup.root().display()
    );

    if options.launch {
        app::launch_app(&seen)?;
        report.launched = true;
    }
    Ok(report)
}

fn stop_app(
    before: &LoginState,
    target: &str,
    options: &SwitchOptions,
    ui: &dyn SwitchUi,
) -> anyhow::Result<()> {
    if !before.signed_in {
        app::kill_app()?;
        wait_for(UNLOCK_TIMEOUT, || (!app::app_is_running()).then_some(()))
            .context("the app did not stop")?;
        return Ok(());
    }
    let deadline = Instant::now() + options.quit_timeout;
    if app::request_quit() {
        log::info!("asked the app to quit");
        let wait = AUTO_QUIT_WAIT.min(options.quit_timeout);
        if wait_for(wait, || (!app::app_is_running()).then_some(())).is_some() {
            return Ok(());
        }
        log::warn!(
            "the app is still running {} s after the quit request",
            wait.as_secs()
        );
    }
    let mut notice = ui.ask_to_quit(target)?;
    while app::app_is_running() {
        if notice.cancelled() {
            bail!("cancelled");
        }
        if Instant::now() >= deadline {
            bail!(
                "the app did not quit within {} s",
                options.quit_timeout.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(())
}

fn wait_for<T>(timeout: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = probe() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
