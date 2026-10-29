//! Login registration on macOS as a launchd user agent in `~/Library/LaunchAgents`.
//!
//! launchd starts the agent at login and restarts it when it exits.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

const LABEL: &str = "io.github.claude-account-sync";

const QUARANTINE_ATTRIBUTE: &str = "com.apple.quarantine";

/// Removes the quarantine attribute from `path` and returns `true` when the file had one.
///
/// `xattr -p` checks for the attribute first, because the error message of `xattr -d` for a missing attribute
/// depends on the system language.
pub fn clear_quarantine(path: &Path) -> anyhow::Result<bool> {
    let present = Command::new("xattr")
        .args(["-p", QUARANTINE_ATTRIBUTE])
        .arg(path)
        .output()
        .context("cannot run xattr")?
        .status
        .success();
    if !present {
        return Ok(false);
    }
    let output = Command::new("xattr")
        .args(["-d", QUARANTINE_ATTRIBUTE])
        .arg(path)
        .output()
        .context("cannot run xattr")?;
    if !output.status.success() {
        bail!(
            "cannot remove {QUARANTINE_ATTRIBUTE} from {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(true)
}

fn plist_path() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().context("cannot find the home folder")?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn domain() -> anyhow::Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("cannot run id -u")?;
    Ok(format!(
        "gui/{}",
        String::from_utf8_lossy(&output.stdout).trim()
    ))
}

pub fn register_and_start(agent: &Path) -> anyhow::Result<()> {
    let plist = plist_path()?;
    std::fs::create_dir_all(plist.parent().expect("plist path has a parent"))?;
    std::fs::write(&plist, render_plist(agent))
        .with_context(|| format!("cannot write {}", plist.display()))?;
    let status = Command::new("launchctl")
        .args(["bootstrap", &domain()?])
        .arg(&plist)
        .status()?;
    if !status.success() {
        bail!("launchctl bootstrap failed for {}", plist.display());
    }
    Ok(())
}

pub fn unregister() -> anyhow::Result<()> {
    let plist = plist_path()?;
    if plist.exists() {
        std::fs::remove_file(&plist)
            .with_context(|| format!("cannot remove {}", plist.display()))?;
    }
    Ok(())
}

/// Unloads the launchd job. launchd stops the agent as part of the unload. Succeeds when the job is not loaded.
pub fn stop_agent(_agent: &Path) -> anyhow::Result<()> {
    let _ = Command::new("launchctl")
        .args(["bootout", &format!("{}/{LABEL}", domain()?)])
        .status();
    Ok(())
}

fn render_plist(agent: &Path) -> String {
    let agent = escape_xml(&agent.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{agent}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ProcessType</key>
  <string>Background</string>
</dict>
</plist>
"#
    )
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
