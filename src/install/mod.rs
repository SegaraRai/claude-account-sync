//! Registers the background watcher to start at login, and removes that registration.
//!
//! `install` copies the CLI and the agent executable into `<state folder>/bin`, registers the agent with the
//! operating system, and starts it. The copies let the user rebuild or delete the original executables while the
//! agent runs. The agent is a separate executable because a Windows console program opens a console window at
//! login, and the agent is built as a windowless program.
//!
//! - `windows`: registers the agent under the current user's `Run` registry key.
//! - `macos`: registers the agent as a launchd user agent.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::state::StateDir;

/// The file name of the agent executable, without the platform's executable suffix.
pub const AGENT_NAME: &str = "claude-account-sync-agent";

/// The file name of the CLI executable, without the platform's executable suffix.
const CLI_NAME: &str = "claude-account-sync";

/// Copies the executables into the state folder, registers the agent to start at login, and starts it.
///
/// The agent executable must be next to the running CLI executable, which is where Cargo builds it. Returns the
/// path of the installed agent.
pub fn install(state_dir: &StateDir) -> anyhow::Result<PathBuf> {
    let current = std::env::current_exe().context("cannot find the running executable")?;
    let source_dir = current
        .parent()
        .context("the running executable has no folder")?;
    let bin_dir = state_dir.root.join("bin");
    let agent = executable(&bin_dir, AGENT_NAME);

    stop_agent(&agent)?;
    std::fs::create_dir_all(&bin_dir)
        .with_context(|| format!("cannot create {}", bin_dir.display()))?;
    for name in [CLI_NAME, AGENT_NAME] {
        let from = executable(source_dir, name);
        let to = executable(&bin_dir, name);
        if from != to {
            std::fs::copy(&from, &to)
                .with_context(|| format!("cannot copy {} to {}", from.display(), to.display()))?;
        }
    }
    register_and_start(&agent)?;
    Ok(agent)
}

/// Removes the login registration and stops the running agent. Keeps the state, backups, logs, and executables.
pub fn uninstall(state_dir: &StateDir) -> anyhow::Result<()> {
    let agent = executable(&state_dir.root.join("bin"), AGENT_NAME);
    unregister()?;
    stop_agent(&agent)
}

fn executable(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

#[cfg(windows)]
use windows::{register_and_start, stop_agent, unregister};

#[cfg(target_os = "macos")]
use macos::{register_and_start, stop_agent, unregister};

#[cfg(not(any(windows, target_os = "macos")))]
fn register_and_start(_agent: &Path) -> anyhow::Result<()> {
    anyhow::bail!("install supports only Windows and macOS")
}

#[cfg(not(any(windows, target_os = "macos")))]
fn unregister() -> anyhow::Result<()> {
    anyhow::bail!("uninstall supports only Windows and macOS")
}

#[cfg(not(any(windows, target_os = "macos")))]
fn stop_agent(_agent: &Path) -> anyhow::Result<()> {
    Ok(())
}
