//! Login registration on Windows through a Task Scheduler task with a logon trigger for the current user.
//!
//! The Microsoft Store version of the Claude desktop app runs Claude Code, and every process that Claude Code
//! starts, inside its MSIX package. Windows redirects the package's writes to `HKCU` and to new files under
//! `AppData` into the package's private storage, so a `Run` registry value or a Startup folder shortcut written
//! from there is invisible at login. Task Scheduler stores tasks in its own service, outside that redirection,
//! and a task that a user registers for their own logon needs no administrator rights.
//!
//! `register_and_start` also starts the agent through Task Scheduler, so the agent runs outside the package even
//! when `install` runs inside it.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, bail};

const TASK_NAME: &str = "claude-account-sync";

pub fn register_and_start(agent: &Path) -> anyhow::Result<()> {
    let user = current_user()?;
    let xml_path = agent.with_file_name("claude-account-sync-task.xml");
    std::fs::write(&xml_path, utf16_with_bom(&render_task(agent, &user)))
        .with_context(|| format!("cannot write {}", xml_path.display()))?;
    let path_text = xml_path.to_string_lossy().into_owned();
    let created = run(
        "schtasks",
        &["/Create", "/TN", TASK_NAME, "/XML", &path_text, "/F"],
    );
    let _ = std::fs::remove_file(&xml_path);
    created?;
    run("schtasks", &["/Run", "/TN", TASK_NAME])
}

pub fn unregister() -> anyhow::Result<()> {
    if task_exists() {
        run("schtasks", &["/Delete", "/TN", TASK_NAME, "/F"])?;
    }
    Ok(())
}

/// Stops the task and every running agent that was started from `agent`. Other copies of the agent keep running.
pub fn stop_agent(agent: &Path) -> anyhow::Result<()> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System, UpdateKind};
    if task_exists() {
        let _ = run("schtasks", &["/End", "/TN", TASK_NAME]);
    }
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

fn task_exists() -> bool {
    Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME])
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Returns the current user as `<domain>\<name>`, which a logon trigger and a task principal take.
fn current_user() -> anyhow::Result<String> {
    let name = std::env::var("USERNAME").context("USERNAME is not set")?;
    Ok(match std::env::var("USERDOMAIN") {
        Ok(domain) if !domain.is_empty() => format!("{domain}\\{name}"),
        _ => name,
    })
}

/// Renders the task definition.
///
/// The task starts at the user's logon with the user's normal rights. It runs while the computer is on battery,
/// has no time limit, and does not start a second instance while one runs.
fn render_task(agent: &Path, user: &str) -> String {
    let agent = escape_xml(&agent.to_string_lossy());
    let user = escape_xml(user);
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Shares Claude desktop Code tab sessions between accounts.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{agent}</Command>
    </Exec>
  </Actions>
</Task>
"#
    )
}

/// `schtasks /XML` requires UTF-16 with a byte order mark.
fn utf16_with_bom(text: &str) -> Vec<u8> {
    std::iter::once(0xFEFF)
        .chain(text.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_escapes_paths_and_encodes_as_utf16() {
        let xml = render_task(Path::new(r"C:\a&b\agent.exe"), r"PC\me");
        assert!(xml.contains(r"<Command>C:\a&amp;b\agent.exe</Command>"));
        assert!(xml.contains(r"<UserId>PC\me</UserId>"));
        let bytes = utf16_with_bom("<a");
        assert_eq!(bytes, [0xFF, 0xFE, b'<', 0, b'a', 0]);
    }
}
