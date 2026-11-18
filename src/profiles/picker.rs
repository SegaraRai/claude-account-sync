//! Asks the user which account to use, and the other questions that a switch needs, in a window or a terminal.
//!
//! The windows use what the operating system already has, so this tool needs no GUI library: Windows Forms
//! through Windows PowerShell on Windows, and AppleScript dialogs through `osascript` on macOS. Text reaches the
//! scripts through environment variables or arguments, never through the script source, so no quoting is needed.

use std::io::{BufRead, Write};
use std::process::{Child, Command, Stdio};
use std::time::SystemTime;

use anyhow::{Context, bail};
use serde::Serialize;

use super::switch::{NoNotice, SwitchUi, WaitNotice};
use super::{LoginState, Profile, format_time};
use crate::app::RunningSession;

/// One account in the picker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PickerRow {
    pub account: String,
    pub name: String,
    /// The email address, when it is known and differs from `name`.
    pub email: Option<String>,
    pub last_used: String,
    pub status: String,
    /// `false` when the account cannot be restored, or is the one that is signed in now.
    pub selectable: bool,
    /// `true` when the user can remove the account from the tool: any saved account but the one signed in now.
    pub removable: bool,
}

/// What the user chose in the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickerChoice {
    /// Switch to this account, or sign in to another one with [`NEW_SIGN_IN`].
    Switch(String),
    /// Remove this account and its saved sign-in from the tool. The user already confirmed it.
    Remove(String),
}

impl PickerRow {
    /// Returns the name with the email address in parentheses, when the address is known and differs.
    pub fn label(&self) -> String {
        match &self.email {
            Some(email) => format!("{} ({email})", self.name),
            None => self.name.clone(),
        }
    }
}

/// The `account` of the picker row that signs in to another account.
pub const NEW_SIGN_IN: &str = "new";

/// Builds the picker rows from the profiles, most recently used first. The most recently used account other than
/// the current one is marked `last used`. While the app is signed in, a last row offers to sign in to another
/// account; its `account` is [`NEW_SIGN_IN`].
pub fn rows(profiles: &[Profile], login: &LoginState, now: SystemTime) -> Vec<PickerRow> {
    let current = login.signed_in_account();
    let mut marked_last = false;
    let mut rows: Vec<PickerRow> = profiles
        .iter()
        .map(|profile| {
            let is_current = current == Some(profile.account.as_str());
            let status = if is_current {
                "signed in now".to_owned()
            } else if !profile.can_restore() {
                if profile.saved_at.is_some() {
                    "signed out: sign in again in the app".to_owned()
                } else {
                    "not saved yet: quit the app once while signed in".to_owned()
                }
            } else if !marked_last {
                marked_last = true;
                "last used".to_owned()
            } else {
                String::new()
            };
            PickerRow {
                account: profile.account.clone(),
                name: profile.display_name(),
                email: profile
                    .email()
                    .filter(|email| *email != profile.display_name())
                    .map(str::to_owned),
                last_used: profile
                    .last_active_at
                    .map_or_else(|| "never".to_owned(), |millis| format_time(millis, now)),
                selectable: !is_current && profile.can_restore(),
                removable: !is_current,
                status,
            }
        })
        .collect();
    if login.signed_in {
        rows.push(PickerRow {
            account: NEW_SIGN_IN.to_owned(),
            name: "+ Sign in to another account".to_owned(),
            email: None,
            last_used: String::new(),
            status: String::new(),
            selectable: true,
            removable: false,
        });
    }
    rows
}

/// Why the picker opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum PickerReason {
    /// The user asked to switch.
    Switch,
    /// The app signed out.
    SignedOut,
}

impl PickerReason {
    fn prompt(self) -> &'static str {
        match self {
            Self::Switch => {
                "Choose the account to switch to. Claude restarts with that account, without signing out of the current one."
            }
            Self::SignedOut => {
                "Claude signed out. Choose an account to sign in with, or cancel to sign in yourself."
            }
        }
    }
}

const TITLE: &str = "Claude account";

/// The interaction of a switch through windows.
pub struct GuiUi;

/// The interaction of a switch through the terminal.
pub struct TerminalUi;

/// Shows the picker window. Returns the user's choice, or `None` when the user cancelled.
pub fn pick_gui(rows: &[PickerRow], reason: PickerReason) -> anyhow::Result<Option<PickerChoice>> {
    if !rows.iter().any(|row| row.selectable || row.removable) {
        alert_gui(&no_choice_message(rows))?;
        return Ok(None);
    }
    let preselect = rows
        .iter()
        .find(|row| row.selectable && row.status == "last used")
        .or_else(|| rows.iter().find(|row| row.selectable))
        .map(|row| row.account.clone());
    if cfg!(windows) {
        let data = serde_json::json!({
            "title": TITLE,
            "prompt": reason.prompt(),
            "rows": rows,
            "preselect": preselect,
            "removeTexts": rows
                .iter()
                .filter(|row| row.removable)
                .map(|row| (row.account.clone(), remove_message(row)))
                .collect::<std::collections::BTreeMap<_, _>>(),
        });
        let output = powershell(PICKER_PS, &[("CLAUDE_ACCOUNT_SYNC_DATA", data.to_string())])
            .stdout(Stdio::piped())
            .output()
            .context("cannot start Windows PowerShell")?;
        Ok(parse_choice(&String::from_utf8_lossy(&output.stdout), rows))
    } else if cfg!(target_os = "macos") {
        pick_macos(rows, reason)
    } else {
        bail!("the picker window supports only Windows and macOS")
    }
}

/// The macOS picker. `choose from list` has no room for another button, so the list ends with an item that
/// opens a second list of the accounts to remove.
fn pick_macos(rows: &[PickerRow], reason: PickerReason) -> anyhow::Result<Option<PickerChoice>> {
    const REMOVE_ITEM: &str = "Remove an account…";
    let choices: Vec<&PickerRow> = rows.iter().filter(|row| row.selectable).collect();
    let mut items: Vec<String> = choices
        .iter()
        .map(|row| {
            if row.account == NEW_SIGN_IN {
                return row.name.clone();
            }
            let mut text = format!("{}  —  last used {}", row.label(), row.last_used);
            if !row.status.is_empty() {
                text += &format!("  ({})", row.status);
            }
            text
        })
        .collect();
    let removable: Vec<&PickerRow> = rows.iter().filter(|row| row.removable).collect();
    if !removable.is_empty() {
        items.push(REMOVE_ITEM.to_owned());
    }
    let Some(index) = choose_macos(reason.prompt(), "Switch", &items)? else {
        return Ok(None);
    };
    if let Some(row) = choices.get(index) {
        return Ok(Some(PickerChoice::Switch(row.account.clone())));
    }
    let items: Vec<String> = removable
        .iter()
        .map(|row| format!("{}  —  last used {}", row.label(), row.last_used))
        .collect();
    let Some(row) = choose_macos("Choose the account to remove.", "Remove…", &items)?
        .and_then(|index| removable.get(index))
    else {
        return Ok(None);
    };
    let confirmed = osascript(
        CONFIRM_AS,
        &[TITLE.into(), remove_message(row), "Remove".into()],
    )
    .status()?
    .success();
    Ok(confirmed.then(|| PickerChoice::Remove(row.account.clone())))
}

/// Shows `items` in a list and returns the index of the chosen one.
fn choose_macos(prompt: &str, ok: &str, items: &[String]) -> anyhow::Result<Option<usize>> {
    let mut args = vec![prompt.to_owned(), ok.to_owned()];
    args.extend(items.iter().cloned());
    let output = osascript(CHOOSE_AS, &args)
        .stdout(Stdio::piped())
        .output()
        .context("cannot start osascript")?;
    let index: Option<usize> = String::from_utf8_lossy(&output.stdout).trim().parse().ok();
    Ok(index.and_then(|index| index.checked_sub(1)))
}

/// Parses what the Windows picker printed: an account to switch to, or `remove:` and an account.
fn parse_choice(output: &str, rows: &[PickerRow]) -> Option<PickerChoice> {
    let output = output.trim();
    let (remove, account) = match output.strip_prefix("remove:") {
        Some(account) => (true, account),
        None => (false, output),
    };
    let row = rows.iter().find(|row| row.account == account)?;
    if remove {
        row.removable
            .then(|| PickerChoice::Remove(row.account.clone()))
    } else {
        row.selectable
            .then(|| PickerChoice::Switch(row.account.clone()))
    }
}

/// The question that confirms removing the account of `row`.
fn remove_message(row: &PickerRow) -> String {
    format!(
        "Remove {} from claude-account-sync?\n\nIts saved sign-in, name, and last-used time are deleted, so you cannot switch to it until you sign in to it in the app again. The account itself and its sessions stay as they are.",
        row.label()
    )
}

/// Asks in the terminal. Returns the user's choice, or `None` when the user entered nothing.
pub fn pick_terminal(rows: &[PickerRow]) -> anyhow::Result<Option<PickerChoice>> {
    if !rows.iter().any(|row| row.selectable || row.removable) {
        bail!("{}", no_choice_message(rows));
    }
    for (index, row) in rows.iter().enumerate() {
        let number = if row.selectable || row.removable {
            format!("{:>2})", index + 1)
        } else {
            "   ".to_owned()
        };
        let status = if row.status.is_empty() {
            String::new()
        } else {
            format!("  [{}]", row.status)
        };
        if row.account == NEW_SIGN_IN {
            println!("{number} {}{status}", row.name);
        } else {
            println!(
                "{number} {}  last used {}  {}{status}",
                row.label(),
                row.last_used,
                row.account
            );
        }
    }
    print!("switch to (number; r and a number to remove; empty to cancel): ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    let (remove, number) = match line.strip_prefix(['r', 'R']) {
        Some(number) => (true, number.trim()),
        None => (false, line),
    };
    let row = number
        .parse::<usize>()
        .ok()
        .and_then(|number| rows.get(number.checked_sub(1)?))
        .filter(|row| {
            if remove {
                row.removable
            } else {
                row.selectable
            }
        })
        .with_context(|| format!("{line:?} is not one of the choices"))?;
    if !remove {
        return Ok(Some(PickerChoice::Switch(row.account.clone())));
    }
    print!("{}\n[y/N] ", remove_message(row));
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes")
        .then(|| PickerChoice::Remove(row.account.clone())))
}

fn no_choice_message(rows: &[PickerRow]) -> String {
    if rows.is_empty() {
        "No account is saved yet. Sign in to each account in the app once, and quit the app while signed in; the watcher saves the sign-in then.".into()
    } else {
        "No other account has a usable saved sign-in. Sign in to the account in the app once, and quit the app while signed in; the watcher saves the sign-in then.".into()
    }
}

fn sessions_message(sessions: &[RunningSession]) -> String {
    let mut text = format!(
        "{} Claude Code session(s) worked in the last {} minutes. Quitting Claude stops them:\n",
        sessions.len(),
        crate::app::ACTIVE_SESSION_WINDOW.as_secs() / 60
    );
    let now = SystemTime::now();
    for session in sessions.iter().take(10) {
        text += &format!(
            "\n  • {}  (last write {})",
            session.title.as_deref().unwrap_or(&session.cli_session_id),
            format_time(session.last_write_at, now)
        );
    }
    if sessions.len() > 10 {
        text += &format!("\n  … and {} more", sessions.len() - 10);
    }
    text + "\n\nA session in the middle of a turn stops there; you can continue it after the switch. Quit and switch anyway?"
}

fn quit_message(target: &str) -> String {
    format!(
        "Claude did not quit on request. Quit it to switch to {}: right-click the Claude icon in the notification area (on macOS, use the Claude menu) and choose Quit.\n\nThe switch continues when Claude has quit, and Claude starts again with that account.",
        target
    )
}

/// Shows an error or information message in a window.
pub fn alert_gui(message: &str) -> anyhow::Result<()> {
    if cfg!(windows) {
        powershell(
            ALERT_PS,
            &[
                ("CLAUDE_ACCOUNT_SYNC_TITLE", TITLE.into()),
                ("CLAUDE_ACCOUNT_SYNC_TEXT", message.into()),
            ],
        )
        .status()?;
    } else if cfg!(target_os = "macos") {
        osascript(ALERT_AS, &[TITLE.into(), message.into()]).status()?;
    }
    Ok(())
}

impl SwitchUi for GuiUi {
    fn confirm_stop_sessions(&self, sessions: &[RunningSession]) -> anyhow::Result<bool> {
        let text = sessions_message(sessions);
        let status = if cfg!(windows) {
            powershell(
                CONFIRM_PS,
                &[
                    ("CLAUDE_ACCOUNT_SYNC_TITLE", TITLE.into()),
                    ("CLAUDE_ACCOUNT_SYNC_TEXT", text),
                ],
            )
            .status()?
        } else {
            osascript(CONFIRM_AS, &[TITLE.into(), text, "Quit and switch".into()]).status()?
        };
        Ok(status.success())
    }

    fn ask_to_quit(&self, target: &str) -> anyhow::Result<Box<dyn WaitNotice>> {
        let text = quit_message(target);
        let child = if cfg!(windows) {
            powershell(
                WAIT_PS,
                &[
                    ("CLAUDE_ACCOUNT_SYNC_TITLE", TITLE.into()),
                    ("CLAUDE_ACCOUNT_SYNC_TEXT", text),
                ],
            )
            .spawn()?
        } else {
            osascript(WAIT_AS, &[TITLE.into(), text]).spawn()?
        };
        Ok(Box::new(ChildNotice { child }))
    }
}

impl SwitchUi for TerminalUi {
    fn confirm_stop_sessions(&self, sessions: &[RunningSession]) -> anyhow::Result<bool> {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            bail!(
                "{} Claude Code session(s) run in the app, and quitting it stops them; pass --yes to switch anyway",
                sessions.len()
            );
        }
        println!("{}", sessions_message(sessions));
        print!("[y/N] ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        Ok(matches!(line.trim(), "y" | "Y" | "yes"))
    }

    fn ask_to_quit(&self, target: &str) -> anyhow::Result<Box<dyn WaitNotice>> {
        println!("{}", quit_message(target));
        Ok(Box::new(NoNotice))
    }
}

/// A notice window that runs as a child process. Closing it cancels; dropping it closes it.
struct ChildNotice {
    child: Child,
}

impl WaitNotice for ChildNotice {
    fn cancelled(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

impl Drop for ChildNotice {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Prepares Windows PowerShell to run `script` without a console window.
fn powershell(script: &str, env: &[(&str, String)]) -> Command {
    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-STA",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
        ])
        .arg(encode_utf16_base64(script))
        .stdin(Stdio::null());
    for (key, value) in env {
        command.env(key, value);
    }
    hide_console(&mut command);
    command
}

fn osascript(script: &str, args: &[String]) -> Command {
    let mut command = Command::new("osascript");
    command
        .arg("-e")
        .arg(script)
        .args(args)
        .stdin(Stdio::null());
    command
}

/// Keeps a console program from opening a console window when a windowless process starts it.
pub fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// Encodes `text` as UTF-16LE in Base64, the format of PowerShell's `-EncodedCommand`.
fn encode_utf16_base64(text: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

const PICKER_PS: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[System.Windows.Forms.Application]::EnableVisualStyles()
$data = $env:CLAUDE_ACCOUNT_SYNC_DATA | ConvertFrom-Json
$form = New-Object System.Windows.Forms.Form
$form.Text = $data.title
$form.StartPosition = 'CenterScreen'
$form.Size = New-Object System.Drawing.Size(800, 360)
$form.MinimumSize = New-Object System.Drawing.Size(420, 240)
$form.TopMost = $true
$form.Font = New-Object System.Drawing.Font('Segoe UI', 9)
$label = New-Object System.Windows.Forms.Label
$label.Text = $data.prompt
$label.Dock = 'Top'
$label.Height = 44
$label.Padding = New-Object System.Windows.Forms.Padding(10, 10, 10, 0)
$list = New-Object System.Windows.Forms.ListView
$list.View = 'Details'
$list.FullRowSelect = $true
$list.MultiSelect = $false
$list.HideSelection = $false
$list.Dock = 'Fill'
[void]$list.Columns.Add('Account', 150)
[void]$list.Columns.Add('Email', 220)
[void]$list.Columns.Add('Last used', 180)
[void]$list.Columns.Add('Status', 200)
foreach ($row in $data.rows) {
  $item = New-Object System.Windows.Forms.ListViewItem($row.name)
  [void]$item.SubItems.Add([string]$row.email)
  [void]$item.SubItems.Add($row.lastUsed)
  [void]$item.SubItems.Add($row.status)
  $item.Tag = $row
  if (-not $row.selectable) { $item.ForeColor = [System.Drawing.SystemColors]::GrayText }
  [void]$list.Items.Add($item)
  if ($row.account -eq $data.preselect) { $item.Selected = $true; $item.Focused = $true }
}
$buttons = New-Object System.Windows.Forms.FlowLayoutPanel
$buttons.Dock = 'Bottom'
$buttons.FlowDirection = 'RightToLeft'
$buttons.Height = 44
$buttons.Padding = New-Object System.Windows.Forms.Padding(6)
$cancel = New-Object System.Windows.Forms.Button -Property @{ Text = 'Cancel'; DialogResult = 'Cancel'; AutoSize = $true }
$ok = New-Object System.Windows.Forms.Button -Property @{ Text = 'Switch'; DialogResult = 'OK'; AutoSize = $true }
$remove = New-Object System.Windows.Forms.Button -Property @{ Text = 'Remove…'; AutoSize = $true }
$remove.Margin = New-Object System.Windows.Forms.Padding(3, 3, 24, 3)
$buttons.Controls.Add($cancel)
$buttons.Controls.Add($ok)
$buttons.Controls.Add($remove)
$script:removed = $null
$update = {
  $selected = $list.SelectedItems.Count -eq 1
  $ok.Enabled = ($selected -and $list.SelectedItems[0].Tag.selectable)
  $remove.Enabled = ($selected -and $list.SelectedItems[0].Tag.removable)
}
$list.add_SelectedIndexChanged($update)
$list.add_DoubleClick({ if ($ok.Enabled) { $form.DialogResult = 'OK'; $form.Close() } })
$remove.add_Click({
  $row = $list.SelectedItems[0].Tag
  $text = $data.removeTexts.($row.account)
  $answer = [System.Windows.Forms.MessageBox]::Show($form, $text, $data.title, 'YesNo', 'Warning', 'Button2')
  if ($answer -eq 'Yes') { $script:removed = $row.account; $form.DialogResult = 'Abort'; $form.Close() }
})
& $update
$form.AcceptButton = $ok
$form.CancelButton = $cancel
$form.Controls.Add($list)
$form.Controls.Add($label)
$form.Controls.Add($buttons)
$form.add_Shown({ $form.Activate(); $list.Focus() })
$result = $form.ShowDialog()
if ($result -eq 'Abort' -and $script:removed) {
  [Console]::Out.Write('remove:' + $script:removed)
} elseif ($result -eq 'OK' -and $list.SelectedItems.Count -eq 1) {
  [Console]::Out.Write($list.SelectedItems[0].Tag.account)
}
"#;

const ALERT_PS: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[System.Windows.Forms.Application]::EnableVisualStyles()
$owner = New-Object System.Windows.Forms.Form -Property @{ TopMost = $true; ShowInTaskbar = $false; StartPosition = 'CenterScreen'; Size = (New-Object System.Drawing.Size(1, 1)); Opacity = 0 }
$owner.Show()
[void][System.Windows.Forms.MessageBox]::Show($owner, $env:CLAUDE_ACCOUNT_SYNC_TEXT, $env:CLAUDE_ACCOUNT_SYNC_TITLE, 'OK', 'Information')
"#;

const CONFIRM_PS: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[System.Windows.Forms.Application]::EnableVisualStyles()
$owner = New-Object System.Windows.Forms.Form -Property @{ TopMost = $true; ShowInTaskbar = $false; StartPosition = 'CenterScreen'; Size = (New-Object System.Drawing.Size(1, 1)); Opacity = 0 }
$owner.Show()
$answer = [System.Windows.Forms.MessageBox]::Show($owner, $env:CLAUDE_ACCOUNT_SYNC_TEXT, $env:CLAUDE_ACCOUNT_SYNC_TITLE, 'YesNo', 'Warning', 'Button2')
if ($answer -eq 'Yes') { exit 0 } else { exit 1 }
"#;

const WAIT_PS: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[System.Windows.Forms.Application]::EnableVisualStyles()
$form = New-Object System.Windows.Forms.Form
$form.Text = $env:CLAUDE_ACCOUNT_SYNC_TITLE
$form.StartPosition = 'CenterScreen'
$form.Size = New-Object System.Drawing.Size(460, 210)
$form.FormBorderStyle = 'FixedDialog'
$form.MaximizeBox = $false
$form.MinimizeBox = $false
$form.TopMost = $true
$form.Font = New-Object System.Drawing.Font('Segoe UI', 9)
$label = New-Object System.Windows.Forms.Label
$label.Text = $env:CLAUDE_ACCOUNT_SYNC_TEXT
$label.Dock = 'Fill'
$label.Padding = New-Object System.Windows.Forms.Padding(12)
$buttons = New-Object System.Windows.Forms.FlowLayoutPanel
$buttons.Dock = 'Bottom'
$buttons.FlowDirection = 'RightToLeft'
$buttons.Height = 44
$buttons.Padding = New-Object System.Windows.Forms.Padding(6)
$cancel = New-Object System.Windows.Forms.Button -Property @{ Text = 'Cancel switch'; DialogResult = 'Cancel'; AutoSize = $true }
$buttons.Controls.Add($cancel)
$form.CancelButton = $cancel
$form.Controls.Add($label)
$form.Controls.Add($buttons)
$form.add_Shown({ $form.Activate() })
[void]$form.ShowDialog()
"#;

const CHOOSE_AS: &str = r#"
on run argv
  set promptText to item 1 of argv
  set okName to item 2 of argv
  set theItems to items 3 thru -1 of argv
  activate
  set picked to choose from list theItems with title "Claude account" with prompt promptText OK button name okName default items {item 1 of theItems}
  if picked is false then return ""
  set p to item 1 of picked
  repeat with i from 1 to count of theItems
    if item i of theItems is p then return i as text
  end repeat
  return ""
end run
"#;

const ALERT_AS: &str = r#"
on run argv
  activate
  display alert (item 1 of argv) message (item 2 of argv)
end run
"#;

const CONFIRM_AS: &str = r#"
on run argv
  activate
  display dialog (item 2 of argv) with title (item 1 of argv) buttons {"Cancel", item 3 of argv} default button "Cancel" cancel button "Cancel" with icon caution
end run
"#;

const WAIT_AS: &str = r#"
on run argv
  activate
  display dialog (item 2 of argv) with title (item 1 of argv) buttons {"Cancel switch"} cancel button "Cancel switch"
end run
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_powershell() {
        // [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes('ab1'))
        assert_eq!(encode_utf16_base64("ab1"), "YQBiADEA");
        assert_eq!(encode_utf16_base64("a"), "YQA=");
        assert_eq!(encode_utf16_base64("é"), "6QA=");
    }

    #[test]
    fn marks_current_last_used_and_unusable_rows() {
        let profile = |account: &str, active: i64, saved: Option<i64>, out: Option<i64>| Profile {
            last_active_at: Some(active),
            saved_at: saved,
            signed_out_at: out,
            ..Profile::new(account)
        };
        let a = "aaaaaaaa-0000-0000-0000-000000000001";
        let b = "bbbbbbbb-0000-0000-0000-000000000002";
        let c = "cccccccc-0000-0000-0000-000000000003";
        let d = "dddddddd-0000-0000-0000-000000000004";
        let profiles = vec![
            profile(a, 40, Some(1), None),
            profile(b, 30, Some(1), Some(2)),
            profile(c, 20, Some(1), None),
            profile(d, 10, Some(1), None),
        ];
        let login = LoginState {
            account: Some(a.into()),
            signed_in: true,
        };
        let rows = rows(&profiles, &login, SystemTime::UNIX_EPOCH);
        assert_eq!(rows.last().unwrap().account, NEW_SIGN_IN);
        let signed_out = LoginState {
            account: Some(a.into()),
            signed_in: false,
        };
        assert!(
            super::rows(&profiles, &signed_out, SystemTime::UNIX_EPOCH)
                .iter()
                .all(|row| row.account != NEW_SIGN_IN)
        );
        let summary: Vec<(bool, &str)> = rows
            .iter()
            .map(|row| (row.selectable, row.status.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                (false, "signed in now"),
                (false, "signed out: sign in again in the app"),
                (true, "last used"),
                (true, ""),
                (true, ""),
            ]
        );
        let removable: Vec<bool> = rows.iter().map(|row| row.removable).collect();
        assert_eq!(removable, [false, true, true, true, false]);

        assert_eq!(
            parse_choice(&format!("{c}\r\n"), &rows),
            Some(PickerChoice::Switch(c.into()))
        );
        assert_eq!(
            parse_choice(&format!("remove:{b}"), &rows),
            Some(PickerChoice::Remove(b.into()))
        );
        assert_eq!(parse_choice(b, &rows), None, "not selectable");
        assert_eq!(
            parse_choice(&format!("remove:{a}"), &rows),
            None,
            "signed in now"
        );
        assert_eq!(parse_choice("", &rows), None);
    }
}
