//! The background watcher that `claude-account-sync install` starts at login, with the account icon in the
//! notification area (the menu bar on macOS).
//!
//! On Windows this is a windowless program, so it opens no console window. It writes its log to `agent.log` in
//! the state folder.
//!
//! The watcher runs on its own thread, because the tray needs the main thread for the platform's event loop. When
//! the user quits from the tray, the whole agent exits, and the watcher with it, until the next login.

#![cfg_attr(windows, windows_subsystem = "windows")]

use claude_account_sync::config::Config;
use claude_account_sync::state::StateDir;
use claude_account_sync::tray::{self, TrayOptions};
use claude_account_sync::{logging, watch};

fn main() {
    let Some(root) = StateDir::default_root() else {
        return;
    };
    let state_dir = StateDir { root };
    if logging::init_file(&state_dir.log_file()).is_err() {
        return;
    }
    // An invalid config does not stop the agent. The watcher skips passes and logs the error until the config is
    // fixed, and this lookup falls back to the default data folder meanwhile.
    let config = Config::load(&state_dir.config_file()).unwrap_or_default();
    let Some(data_dir) = config.resolve_data_dir() else {
        log::error!("cannot find the Claude desktop app's data folder");
        return;
    };
    // A second agent would only fail to take the watcher's lock. Leave before it shows a second icon.
    match state_dir.lock_watcher() {
        Ok(lock) => drop(lock),
        Err(error) => {
            log::error!("{error}");
            return;
        }
    }
    let watcher = {
        let (data_dir, state_dir) = (data_dir.clone(), state_dir.clone());
        std::thread::spawn(move || {
            if let Err(error) = watch::run(&data_dir, &state_dir, None) {
                log::error!("watcher stopped: {error:#}");
                std::process::exit(1);
            }
        })
    };
    if config.tray_icon {
        match tray::run(data_dir, state_dir, TrayOptions::default()) {
            Ok(()) => std::process::exit(0),
            Err(error) => log::error!("no tray icon: {error:#}"),
        }
    }
    let _ = watcher.join();
}
