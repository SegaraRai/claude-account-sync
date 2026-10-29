//! Shares the Claude desktop app's Code tab data between accounts that sign in on the same computer.
//!
//! The app stores sidebar session records, the task backlog, and local scheduled tasks in one folder per account
//! and organization, called a replica here. Transcripts, settings, and memory live under `~/.claude` and do not
//! depend on the account. This crate merges the per-replica data so that every account shows the same sessions.
//!
//! The crate does not sync claude.ai chats, Projects, Artifacts, Cowork sessions, connectors, or anything on
//! another computer.
//!
//! [`pass::run`] runs one merge. [`watch::run`] repeats it after each change. [`install::install`] starts the
//! watcher at login.
//!
//! Modules:
//!
//! - [`config`]: reads and validates the user's settings, which select collections and replicas.
//! - [`app`]: locates the app's data folder and reads whether the app runs and which account it shows.
//! - [`replica`]: lists replica folders.
//! - [`merge`]: decides, for each key, which version every replica should hold.
//! - [`state`]: stores what the previous pass left in each replica, and locks the tool's folder.
//! - [`pass`]: runs one pass over all collections and reports the changes.
//! - [`sessions`], [`backlog`], [`scheduled_tasks`]: define the schema of each collection and apply its changes.
//! - [`item_file`]: merges collections that the app stores as an item array in one JSON file.
//! - [`file_ops`]: reads and writes files atomically and keeps backups.
//! - [`watch`], [`install`], [`logging`]: run the background watcher.
//! - [`skill`]: installs the Claude Code skill that describes the CLI.

pub mod app;
pub mod backlog;
pub mod config;
pub mod file_ops;
pub mod install;
pub mod item_file;
pub mod logging;
pub mod merge;
pub mod pass;
pub mod replica;
pub mod scheduled_tasks;
pub mod sessions;
pub mod skill;
pub mod state;
pub mod watch;
