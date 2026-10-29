//! Login registration on Windows through `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//!
//! The `Run` key needs no administrator rights, and Windows starts its entries after the user logs in.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "claude-account-sync";

/// Starts the process without a console and outside the console's process group, so it keeps running after the
/// console closes.
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

pub fn register_and_start(agent: &Path) -> anyhow::Result<()> {
    let command = format!("\"{}\"", agent.display());
    run(
        "reg",
        &[
            "add", RUN_KEY, "/v", VALUE_NAME, "/t", "REG_SZ", "/d", &command, "/f",
        ],
    )?;
    Command::new(agent)
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("cannot start {}", agent.display()))?;
    Ok(())
}

pub fn unregister() -> anyhow::Result<()> {
    let status = Command::new("reg")
        .args(["query", RUN_KEY, "/v", VALUE_NAME])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        run("reg", &["delete", RUN_KEY, "/v", VALUE_NAME, "/f"])?;
    }
    Ok(())
}

/// Stops running agents that were started from `agent`. Other copies of the agent keep running.
pub fn stop_agent(agent: &Path) -> anyhow::Result<()> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System, UpdateKind};
    let system = System::new_with_specifics(
        RefreshKind::nothing()
            .with_processes(ProcessRefreshKind::nothing().with_exe(UpdateKind::Always)),
    )
    .map_err(|error| anyhow::anyhow!("cannot list processes: {error:?}"))?;
    for process in system.processes().values() {
        if process.exe().is_some_and(|exe| same_path(exe, agent)) {
            process.kill();
            process.wait();
        }
    }
    Ok(())
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}

fn run(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("cannot run {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}
