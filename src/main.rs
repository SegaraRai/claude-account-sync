use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::json;

use claude_account_sync::app::{AppStatus, SESSIONS_DIR};
use claude_account_sync::config::{Config, ConfigKey, ReplicaPattern};
use claude_account_sync::file_ops::{self, BackupUsage};
use claude_account_sync::pass::{self, PassOptions, Report};
use claude_account_sync::profiles::picker::{self, GuiUi, PickerChoice, PickerReason, TerminalUi};
use claude_account_sync::profiles::switch::{self, SwitchOptions, SwitchUi, Target};
use claude_account_sync::profiles::{CaptureOutcome, LoginState, ProfileStore};
use claude_account_sync::replica::{self, ReplicaId};
use claude_account_sync::state::StateDir;
use claude_account_sync::{install, logging, sessions, watch};

/// Shares Claude desktop Code tab sessions, backlog, and scheduled tasks between accounts on this computer.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// The app's data folder. Overrides `dataDir` in the config. Defaults to `%APPDATA%\Claude` on Windows and
    /// `~/Library/Application Support/Claude` on macOS.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// This tool's folder for its config, state, backups, and logs.
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    /// Prints machine-readable JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Shows the app's status, the replicas, and the changes that a sync would make.
    Status,
    /// Lists the account and organization folders, with their session counts.
    Accounts,
    /// Runs one sync pass.
    Sync {
        /// Lists the changes without making them.
        #[arg(long)]
        dry_run: bool,
    },
    /// Syncs after every change until stopped.
    Watch {
        /// Seconds between passes when no change arrives. Overrides `watchIntervalSecs` in the config.
        #[arg(long)]
        interval: Option<u64>,
    },
    /// Starts the watcher at login and starts it now. Also updates an installed copy.
    Install,
    /// Stops the watcher and removes its login registration.
    Uninstall,
    /// Shows or changes the config.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Shows the account icon in the notification area (the menu bar on macOS) until you quit it. The installed
    /// agent shows it at login, so this is for trying it out.
    Tray {
        /// Starts every switch from the menu with `--dry-run`, so that nothing changes.
        #[arg(long)]
        dry_run: bool,
    },
    /// Lists the accounts whose sign-in is saved, or changes them.
    Profiles {
        #[command(subcommand)]
        command: Option<ProfilesCommand>,
    },
    /// Switches the app to another saved account without signing out. Quits and restarts the app.
    Switch {
        /// The account: its UUID, the start of its UUID, or its label. Without it, a picker asks.
        #[arg(conflicts_with = "new")]
        target: Option<String>,
        /// Saves and removes the current sign-in without signing out, and opens the app's sign-in page, so that
        /// another account can sign in.
        #[arg(long)]
        new: bool,
        /// Asks in windows instead of the terminal.
        #[arg(long)]
        gui: bool,
        /// Why the picker opens, which changes its message.
        #[arg(long, value_enum, default_value = "switch")]
        reason: PickerReason,
        /// Quits running Claude Code sessions without asking.
        #[arg(long)]
        yes: bool,
        /// Leaves the app stopped after the switch.
        #[arg(long)]
        no_launch: bool,
        /// Seconds to wait for the app to quit.
        #[arg(long, default_value_t = 600)]
        quit_timeout_secs: u64,
        /// Asks the questions and shows the plan, but neither stops the app nor changes files.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum ProfilesCommand {
    /// Lists the saved accounts, most recently used first.
    List,
    /// Saves the current sign-in now. The app must be stopped.
    Save,
    /// Names an account, or removes its name when no label is given.
    Label {
        account: String,
        label: Option<String>,
    },
    /// Deletes an account's saved sign-in.
    Forget { account: String },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Prints the config, including defaults for missing fields.
    Show,
    /// Prints the path of the config file.
    Path,
    /// Sets one setting.
    Set { key: ConfigKey, value: String },
    /// Sets one setting back to its default.
    Reset { key: ConfigKey },
    /// Stops syncing an account (`<account>`) or one of its organizations (`<account>/<org>`).
    Exclude { pattern: ReplicaPattern },
    /// Removes an entry that `exclude` added.
    Include { pattern: ReplicaPattern },
}

fn main() -> ExitCode {
    logging::init_stderr();
    let cli = Cli::parse();
    let json = cli.json;
    match run(cli) {
        Ok(code) => code,
        Err(error) => {
            if json {
                println!("{}", json!({ "error": format!("{error:#}") }));
            } else {
                eprintln!("error: {error:#}");
            }
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let state_dir = StateDir {
        root: match cli.state_dir {
            Some(dir) => dir,
            None => {
                StateDir::default_root().context("cannot find a folder for this tool's state")?
            }
        },
    };
    let config_path = state_dir.config_file();
    let data_dir = || -> anyhow::Result<PathBuf> {
        if let Some(dir) = &cli.data_dir {
            return Ok(dir.clone());
        }
        Config::load(&config_path)?.resolve_data_dir().context(
            "cannot find the Claude desktop app's data folder; set it with `config set data-dir`",
        )
    };

    match cli.command {
        Command::Status => {
            let data_dir = data_dir()?;
            let config = Config::load(&config_path)?;
            let status = AppStatus::read(&data_dir);
            let report = pass::run(
                &data_dir,
                &state_dir,
                &config,
                status.clone(),
                PassOptions { dry_run: true },
            )?;
            let backups = file_ops::backup_usage(&state_dir.backups_dir())
                .with_context(|| format!("cannot measure {}", state_dir.backups_dir().display()))?;
            if cli.json {
                print_json(&json!({
                    "dataDir": data_dir,
                    "stateDir": state_dir.root,
                    "configPath": config_path,
                    "app": status,
                    "backups": backups,
                    "report": report,
                }));
            } else {
                println!("data folder:    {}", data_dir.display());
                println!("state folder:   {}", state_dir.root.display());
                println!("app running:    {}", status.running);
                println!(
                    "active account: {}",
                    status.active_account.as_deref().unwrap_or("(unknown)")
                );
                print_backups(&backups);
                print_report(&report, true);
            }
        }
        Command::Accounts => {
            let data_dir = data_dir()?;
            let config = Config::load(&config_path)?;
            let rows = account_rows(&data_dir, &config)?;
            if cli.json {
                print_json(&rows);
            } else {
                for row in &rows {
                    let mut notes = Vec::new();
                    if row.active {
                        notes.push("active");
                    }
                    if !row.writable {
                        notes.push("read-only while the app runs");
                    }
                    if row.excluded {
                        notes.push("excluded");
                    }
                    println!(
                        "{}  {} session(s)  {}",
                        row.id,
                        row.sessions,
                        notes.join(", ")
                    );
                }
            }
        }
        Command::Sync { dry_run } => {
            let data_dir = data_dir()?;
            let config = Config::load(&config_path)?;
            let status = AppStatus::read(&data_dir);
            let report = pass::run(
                &data_dir,
                &state_dir,
                &config,
                status,
                PassOptions { dry_run },
            )?;
            if cli.json {
                print_json(&json!({ "dryRun": dry_run, "report": report }));
            } else {
                print_report(&report, dry_run);
            }
            if !report.problems.is_empty() {
                return Ok(ExitCode::from(2));
            }
        }
        Command::Watch { interval } => {
            let interval = interval.map(|secs| Duration::from_secs(secs.max(5)));
            watch::run(&data_dir()?, &state_dir, interval)?;
        }
        Command::Install => {
            let installed = install::install(&state_dir)?;
            print_done(
                cli.json,
                json!({ "installed": installed, "log": state_dir.log_file() }),
                || {
                    let mut text = format!("installed and started {}", installed.agent.display());
                    for path in &installed.cleared_quarantine {
                        text +=
                            &format!("\nremoved the quarantine attribute from {}", path.display());
                    }
                    text + &format!("\nlog: {}", state_dir.log_file().display())
                },
            );
        }
        Command::Uninstall => {
            install::uninstall(&state_dir)?;
            print_done(cli.json, json!({ "uninstalled": true }), || {
                "stopped the watcher and removed its login registration".into()
            });
        }
        Command::Config(command) => return run_config(command, &config_path, cli.json),
        Command::Tray { dry_run } => {
            claude_account_sync::tray::run(
                data_dir()?,
                state_dir.clone(),
                claude_account_sync::tray::TrayOptions { dry_run },
            )?;
        }
        Command::Profiles { command } => run_profiles(
            command.unwrap_or(ProfilesCommand::List),
            &data_dir()?,
            &state_dir,
            cli.json,
        )?,
        Command::Switch {
            target,
            new,
            gui,
            reason,
            yes,
            no_launch,
            quit_timeout_secs,
            dry_run,
        } => {
            let options = SwitchOptions {
                quit_timeout: Duration::from_secs(quit_timeout_secs),
                launch: !no_launch,
                yes,
                dry_run,
            };
            let result = data_dir().and_then(|data_dir| {
                run_switch(
                    target, new, gui, reason, &options, &data_dir, &state_dir, cli.json,
                )
            });
            if gui
                && let Err(error) = &result
                && format!("{error:#}") != "cancelled"
            {
                let _ = picker::alert_gui(&format!("The account switch failed: {error:#}"));
            }
            result?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn run_config(command: ConfigCommand, path: &Path, json: bool) -> anyhow::Result<ExitCode> {
    let mut config = Config::load(path)?;
    match command {
        ConfigCommand::Show => {
            if json {
                print_json(&config);
            } else {
                println!("{}", serde_json::to_string_pretty(&config)?);
            }
            return Ok(ExitCode::SUCCESS);
        }
        ConfigCommand::Path => {
            print_done(json, json!({ "configPath": path }), || {
                path.display().to_string()
            });
            return Ok(ExitCode::SUCCESS);
        }
        ConfigCommand::Set { key, value } => config.set(key, &value)?,
        ConfigCommand::Reset { key } => config.reset(key),
        ConfigCommand::Exclude { pattern } => {
            config.excluded.insert(pattern);
        }
        ConfigCommand::Include { pattern } => {
            if !config.excluded.remove(&pattern) {
                anyhow::bail!("{pattern} is not in the excluded list");
            }
        }
    }
    config.save(path)?;
    if json {
        print_json(&config);
    } else {
        println!("saved {}", path.display());
    }
    Ok(ExitCode::SUCCESS)
}

fn run_profiles(
    command: ProfilesCommand,
    data_dir: &Path,
    state_dir: &StateDir,
    json: bool,
) -> anyhow::Result<()> {
    let store = ProfileStore::new(&state_dir.root);
    match command {
        ProfilesCommand::List => {
            store.refresh_identities(data_dir);
            let login = LoginState::read(data_dir);
            let mut rows = picker::rows(&store.list()?, &login, std::time::SystemTime::now());
            rows.retain(|row| row.account != picker::NEW_SIGN_IN);
            if json {
                print_json(&json!({ "login": login, "profiles": rows }));
            } else if rows.is_empty() {
                println!(
                    "no saved accounts yet; the watcher saves a sign-in when the app quits while signed in"
                );
            } else {
                for row in &rows {
                    let status = if row.status.is_empty() {
                        String::new()
                    } else {
                        format!("  [{}]", row.status)
                    };
                    println!(
                        "{}  {}  last used {}{status}",
                        row.account,
                        row.label(),
                        row.last_used
                    );
                }
            }
        }
        ProfilesCommand::Save => {
            anyhow::ensure!(
                !claude_account_sync::app::app_is_running(),
                "quit the app first; it locks its cookie database while it runs"
            );
            let login = LoginState::read(data_dir);
            let account = login
                .signed_in_account()
                .context("the app is not signed in")?
                .to_owned();
            let outcome = store.capture(data_dir, &account, std::time::SystemTime::now())?;
            let saved = outcome == CaptureOutcome::Saved;
            print_done(json, json!({ "account": account, "saved": saved }), || {
                if saved {
                    format!("saved the sign-in of {account}")
                } else {
                    format!("the saved sign-in of {account} is up to date")
                }
            });
        }
        ProfilesCommand::Label { account, label } => {
            let profile = store.resolve(&account)?;
            store.set_label(&profile.account, label)?;
            print_done(json, json!({ "account": profile.account }), || {
                "saved".into()
            });
        }
        ProfilesCommand::Forget { account } => {
            let profile = store.resolve(&account)?;
            store.forget(&profile.account)?;
            print_done(json, json!({ "forgot": profile.account }), || {
                format!("deleted the saved sign-in of {}", profile.account)
            });
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_switch(
    target: Option<String>,
    new: bool,
    gui: bool,
    reason: PickerReason,
    options: &SwitchOptions,
    data_dir: &Path,
    state_dir: &StateDir,
    json: bool,
) -> anyhow::Result<()> {
    use std::io::IsTerminal;
    let store = ProfileStore::new(&state_dir.root);
    let account = match target {
        _ if new => picker::NEW_SIGN_IN.to_owned(),
        Some(query) => store.resolve(&query)?.account,
        None => {
            if !gui && !std::io::stdin().is_terminal() {
                anyhow::bail!("name the account to switch to, or pass --gui; see `profiles`");
            }
            // Removing an account shows the picker again, without that account.
            loop {
                store.refresh_identities(data_dir);
                let login = LoginState::read(data_dir);
                let rows = picker::rows(&store.list()?, &login, std::time::SystemTime::now());
                let chosen = if gui {
                    picker::pick_gui(&rows, reason)?
                } else {
                    picker::pick_terminal(&rows)?
                };
                match chosen.context("cancelled")? {
                    PickerChoice::Switch(account) => break account,
                    PickerChoice::Remove(account) => {
                        store.forget(&account)?;
                        log::info!("removed the saved account {account}");
                        if !gui {
                            println!("removed {account}");
                        }
                    }
                }
            }
        }
    };
    let ui: &dyn SwitchUi = if gui { &GuiUi } else { &TerminalUi };
    let target = if account == picker::NEW_SIGN_IN {
        Target::NewSignIn
    } else {
        Target::Account(account.clone())
    };
    let report = switch::switch(data_dir, state_dir, &target, options, ui)?;
    let destination = match &target {
        Target::Account(account) => account.clone(),
        Target::NewSignIn => "the sign-in page".to_owned(),
    };
    print_done(json, serde_json::to_value(&report)?, || {
        if report.already_active {
            format!("{account} is already signed in")
        } else if report.dry_run {
            let stop = match report.stop {
                Some("kill") => "kill the signed-out app, ",
                Some("quit") => "ask the app to quit, ",
                Some(_) => "wait for you to quit the app, ",
                None => "",
            };
            format!(
                "would {stop}save the current sign-in, switch to {destination}, and start the app; {} session(s) would stop",
                report.sessions_to_stop.len()
            )
        } else {
            let mut text = format!("switched to {destination}");
            if let Some(dir) = &report.backup_dir {
                text += &format!("\nbackup: {}", dir.display());
            }
            text
        }
    });
    Ok(())
}

/// One row of `accounts`.
#[derive(Serialize)]
struct AccountRow {
    #[serde(flatten)]
    id: ReplicaId,
    active: bool,
    writable: bool,
    excluded: bool,
    /// Number of readable and unreadable session record files.
    sessions: usize,
}

fn account_rows(data_dir: &Path, config: &Config) -> anyhow::Result<Vec<AccountRow>> {
    let status = AppStatus::read(data_dir);
    let sessions_root = data_dir.join(SESSIONS_DIR);
    let ids = replica::discover(&sessions_root)
        .with_context(|| format!("cannot list {}", sessions_root.display()))?;
    Ok(ids
        .into_iter()
        .map(|id| {
            let dir = sessions_root.join(&id.account).join(&id.org);
            AccountRow {
                active: status.active_account.as_deref() == Some(id.account.as_str()),
                writable: status.may_write(&id.account),
                excluded: config.is_excluded(&id),
                sessions: sessions::count_records(&dir),
                id,
            }
        })
        .collect())
}

fn print_json(value: &impl Serialize) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("output serializes to JSON")
    );
}

fn print_done(json: bool, value: serde_json::Value, text: impl FnOnce() -> String) {
    if json {
        print_json(&value);
    } else {
        println!("{}", text());
    }
}

fn print_backups(usage: &BackupUsage) {
    println!("backup folder:  {}", usage.dir.display());
    let Some(newest) = usage.folders.last() else {
        println!("  no backups yet");
        return;
    };
    println!(
        "  {} backup(s), {} in total, newest {}",
        usage.folders.len(),
        format_bytes(usage.total_bytes),
        newest.name
    );
    for folder in usage.folders.iter().filter(|folder| folder.initial) {
        println!(
            "  {} holds the data from before the first sync ({}); pruning keeps it",
            folder.name,
            format_bytes(folder.bytes)
        );
    }
}

/// Formats a byte count with binary units, such as `1.5 MiB`.
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn print_report(report: &Report, dry_run: bool) {
    println!("replicas:");
    for replica in &report.replicas {
        let note = if replica.writable {
            ""
        } else {
            " (read-only while the app runs)"
        };
        println!("  {}{note}", replica.id);
    }
    for id in &report.excluded {
        println!("  {id} (excluded)");
    }
    if !report.disabled_collections.is_empty() {
        println!("disabled: {}", report.disabled_collections.join(", "));
    }
    let verb = if dry_run { "planned" } else { "applied" };
    println!("{} {verb} change(s)", report.changes.len());
    for change in &report.changes {
        println!("  {change}");
    }
    for problem in &report.problems {
        println!("problem: {problem}");
    }
    if let Some(dir) = &report.backup_dir {
        println!("backup: {}", dir.display());
    }
}
