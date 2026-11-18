//! The watcher's part of account switching.
//!
//! After each pass the watcher reports whether the app runs and which account is signed in. From this the
//! observer:
//!
//! - saves the sign-in while the app is stopped, so that `switch` can restore it later;
//! - records when each account was last used, and its name and email address from the CLI's profile cache;
//! - notices when the running app signs out, marks that account's saved sign-in as revoked, and opens the picker.
//!
//! A sign-out counts only when the app still shows no sign-in [`SIGN_OUT_CONFIRM`] later, so that a short gap
//! while the app rewrites `config.json` is not taken for one. It also does not count when the account's sign-in
//! was saved after the observer last saw it signed in: then `switch --new` stopped the app and removed the sign-in
//! without signing out, and the observer may simply have missed the time when the app was stopped.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use super::{CaptureOutcome, LoginState, ProfileStore};
use crate::config::Config;
use crate::state::StateDir;

/// How long the app must show no sign-in before the observer treats it as signed out.
pub const SIGN_OUT_CONFIRM: Duration = Duration::from_secs(5);

/// How often the last-used time of the active account is written while the app runs.
const TOUCH_EVERY: Duration = Duration::from_secs(300);

/// The app's state at one moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub running: bool,
    pub login: LoginState,
}

/// What the observer decided to do after one observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Save the sign-in of this account. The app is stopped.
    Capture(String),
    /// Record that this account is in use now.
    TouchActive(String),
    /// The app signed out of this account. `picker` is `true` when the app still runs and shows its sign-in page.
    SignedOut { account: String, picker: bool },
}

#[derive(Default)]
pub struct Observer {
    previous: Option<Observation>,
    pending_sign_out: Option<(String, Instant)>,
    last_touch: Option<(String, Instant)>,
    /// When the observer last saw the running app signed in.
    last_signed_in_at: Option<SystemTime>,
}

impl Observer {
    /// Decides the actions for `observation`. Also returns how soon the observer wants to look again, when it waits
    /// to confirm a sign-out.
    pub fn step(
        &mut self,
        observation: Observation,
        now: Instant,
    ) -> (Vec<Action>, Option<Duration>) {
        let mut actions = Vec::new();
        let mut recheck = None;
        let previous_account = self.previous.as_ref().and_then(|previous| {
            previous
                .login
                .signed_in_account()
                .map(|account| (previous.running, account.to_owned()))
        });
        match (observation.running, observation.login.signed_in_account()) {
            (false, Some(account)) => {
                actions.push(Action::Capture(account.to_owned()));
                self.pending_sign_out = None;
            }
            (true, Some(account)) => {
                let due = match &self.last_touch {
                    Some((last, at)) => last != account || now.duration_since(*at) >= TOUCH_EVERY,
                    None => true,
                };
                if due {
                    actions.push(Action::TouchActive(account.to_owned()));
                    self.last_touch = Some((account.to_owned(), now));
                }
                self.pending_sign_out = None;
            }
            (running, None) => {
                if let Some((true, account)) = previous_account {
                    if running {
                        self.pending_sign_out = Some((account, now));
                        recheck = Some(SIGN_OUT_CONFIRM);
                    } else {
                        // Signed out and quit at once.
                        actions.push(Action::SignedOut {
                            account,
                            picker: false,
                        });
                    }
                } else if let Some((account, since)) = self.pending_sign_out.take() {
                    let waited = now.duration_since(since);
                    if waited >= SIGN_OUT_CONFIRM {
                        actions.push(Action::SignedOut {
                            account,
                            picker: running,
                        });
                    } else {
                        recheck = Some(SIGN_OUT_CONFIRM - waited);
                        self.pending_sign_out = Some((account, since));
                    }
                }
            }
        }
        self.previous = Some(observation);
        (actions, recheck)
    }

    /// Observes the app, carries out the actions, and returns how soon to look again.
    pub fn observe(
        &mut self,
        data_dir: &Path,
        state_dir: &StateDir,
        config: &Config,
    ) -> Option<Duration> {
        if !config.save_sign_ins {
            return None;
        }
        let observation = Observation {
            running: crate::app::app_is_running(),
            login: LoginState::read(data_dir),
        };
        let now = SystemTime::now();
        if observation.running && observation.login.signed_in {
            self.last_signed_in_at = Some(now);
        }
        let (actions, recheck) = self.step(observation, Instant::now());
        let store = ProfileStore::new(&state_dir.root);
        if let Some((account, identity)) = super::read_cli_identity() {
            match store.record_identity(&account, &identity) {
                Ok(true) => log::info!("learned the name of account {account}"),
                Ok(false) => {}
                Err(error) => log::warn!("cannot store the name of {account}: {error}"),
            }
        }
        for action in actions {
            let result = match &action {
                Action::Capture(account) => {
                    store
                        .capture(data_dir, account, now)
                        .map(|outcome| match outcome {
                            CaptureOutcome::Saved => log::info!("saved the sign-in of {account}"),
                            CaptureOutcome::Unchanged => {}
                        })
                }
                Action::TouchActive(account) => store.touch_active(account, now),
                Action::SignedOut { account, .. }
                    if moved_by_switch(&store, account, self.last_signed_in_at) =>
                {
                    log::info!("a switch moved the sign-in of {account} away; it stays usable");
                    Ok(())
                }
                Action::SignedOut { account, picker } => {
                    log::info!(
                        "the app signed out of {account}; its saved sign-in is likely revoked"
                    );
                    let marked = store.mark_signed_out(account, now);
                    if *picker && config.picker_on_sign_out {
                        open_picker(data_dir, state_dir);
                    }
                    marked
                }
            };
            if let Err(error) = result {
                log::warn!("{action:?} failed: {error}");
            }
        }
        recheck
    }
}

/// Returns `true` when `account` was saved after the observer last saw the app signed in to it. The app then
/// stopped and a switch saved its sign-in and removed it, for example to sign in to another account. The app did
/// not sign out, so the saved sign-in is still valid.
fn moved_by_switch(
    store: &ProfileStore,
    account: &str,
    last_signed_in_at: Option<SystemTime>,
) -> bool {
    let Some(seen) = last_signed_in_at else {
        return false;
    };
    store
        .load(account)
        .ok()
        .flatten()
        .and_then(|profile| profile.saved_at)
        .is_some_and(|saved| saved >= crate::file_ops::unix_millis(seen))
}

/// Starts `claude-account-sync switch --gui --reason signed-out` in the background.
fn open_picker(data_dir: &Path, state_dir: &StateDir) {
    // A running switch holds this lock, and its own window is already open.
    match state_dir.lock_named("switch.lock") {
        Ok(lock) => drop(lock),
        Err(_) => return,
    }
    super::spawn_cli(
        data_dir,
        state_dir,
        &["switch", "--gui", "--reason", "signed-out"],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaa-0000-0000-0000-000000000001";

    fn observation(running: bool, signed_in: bool) -> Observation {
        Observation {
            running,
            login: LoginState {
                account: Some(A.into()),
                signed_in,
            },
        }
    }

    #[test]
    fn captures_while_stopped_and_touches_while_running() {
        let mut observer = Observer::default();
        let t = Instant::now();
        let (actions, _) = observer.step(observation(false, true), t);
        assert_eq!(actions, [Action::Capture(A.into())]);
        let (actions, _) = observer.step(observation(true, true), t);
        assert_eq!(actions, [Action::TouchActive(A.into())]);
        let (actions, _) = observer.step(observation(true, true), t + Duration::from_secs(10));
        assert!(actions.is_empty());
        let (actions, _) = observer.step(observation(true, true), t + TOUCH_EVERY);
        assert_eq!(actions, [Action::TouchActive(A.into())]);
    }

    #[test]
    fn confirms_sign_out_after_a_delay() {
        let mut observer = Observer::default();
        let t = Instant::now();
        observer.step(observation(true, true), t);
        let (actions, recheck) = observer.step(observation(true, false), t);
        assert!(actions.is_empty());
        assert_eq!(recheck, Some(SIGN_OUT_CONFIRM));
        let (actions, recheck) =
            observer.step(observation(true, false), t + Duration::from_secs(2));
        assert!(actions.is_empty());
        assert_eq!(recheck, Some(SIGN_OUT_CONFIRM - Duration::from_secs(2)));
        let (actions, recheck) = observer.step(observation(true, false), t + SIGN_OUT_CONFIRM);
        assert_eq!(
            actions,
            [Action::SignedOut {
                account: A.into(),
                picker: true
            }]
        );
        assert_eq!(recheck, None);
        // It fires once.
        let (actions, _) = observer.step(observation(true, false), t + SIGN_OUT_CONFIRM * 3);
        assert!(actions.is_empty());
    }

    #[test]
    fn short_gap_is_not_a_sign_out() {
        let mut observer = Observer::default();
        let t = Instant::now();
        observer.step(observation(true, true), t);
        observer.step(observation(true, false), t);
        observer.step(observation(true, true), t + Duration::from_secs(1));
        let (actions, _) = observer.step(observation(true, false), t + SIGN_OUT_CONFIRM * 2);
        assert!(actions.is_empty(), "a new gap starts a new wait");
    }

    #[test]
    fn sign_in_saved_after_last_seen_is_moved_not_signed_out() {
        let state = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(state.path());
        let seen = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        assert!(!moved_by_switch(&store, A, Some(seen)), "no profile");
        let mut profile = super::super::Profile::new(A);
        profile.saved_at = Some(crate::file_ops::unix_millis(seen) - 1);
        store.save_meta(&profile).unwrap();
        assert!(!moved_by_switch(&store, A, Some(seen)), "saved before");
        profile.saved_at = Some(crate::file_ops::unix_millis(seen) + 1);
        store.save_meta(&profile).unwrap();
        assert!(moved_by_switch(&store, A, Some(seen)));
        assert!(!moved_by_switch(&store, A, None));
    }

    #[test]
    fn app_starting_signed_out_is_not_a_sign_out() {
        let mut observer = Observer::default();
        let t = Instant::now();
        let (actions, recheck) = observer.step(observation(true, false), t);
        assert!(actions.is_empty());
        assert_eq!(recheck, None);
    }
}
