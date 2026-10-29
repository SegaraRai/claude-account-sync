//! The background watcher that `claude-account-sync install` starts at login.
//!
//! On Windows this is a windowless program, so it opens no console window. It writes its log to `agent.log` in
//! the state folder.

#![cfg_attr(windows, windows_subsystem = "windows")]

use claude_account_sync::config::Config;
use claude_account_sync::state::StateDir;
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
    if let Err(error) = watch::run(&data_dir, &state_dir, None) {
        log::error!("watcher stopped: {error:#}");
    }
}
