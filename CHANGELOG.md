# Changelog

This file lists the changes that users notice in each release. [docs/releasing.md](docs/releasing.md) describes how to add entries and cut a release.

## Unreleased

## [0.2.0] - 2026-11-19

The question before a switch now lists only the Claude Code sessions that are still working.

### Breaking changes

- **Switching:** In `switch --json`, each entry of `sessionsToStop` has `busy`, `backgroundTasks`, and `lastActivityAt` instead of `lastWriteAt`. Update scripts that read `lastWriteAt`.

### Fixes

- **Switching:** Ask before quitting the app only for Claude Code sessions that are in the middle of a turn or have background tasks running, such as commands, monitors, or agents. Sessions that finished a few minutes ago no longer trigger the question.

## [0.1.0] - 2026-11-19

First release: share Code tab sessions between the accounts on one computer, and switch the desktop app between them without signing out.

### Features

- **Sync:** Merge the Code tab's sidebar sessions, task backlog, and local scheduled tasks between all accounts that sign in to the Claude desktop app on the same computer. Edits and deletions in one account reach the others, and replaced files are backed up first.
- **Switching:** Save each account's sign-in and switch the app between accounts without signing out, with `switch [<account>]`. The tool quits the app, restores the sign-in, and starts the app again, and asks first when Code tab sessions are running.
- **Switching:** Add another account without signing out of the current one, with `switch --new`.
- **Switching:** Show an account picker with each account's name, email, and last use when the user signs out of the app.
- **Switching:** Remove a saved account from the picker with **Remove…**, or with `profiles forget`.
- **Switching:** Show each account's name and email address, kept up to date from the app's own cache of the signed-in account.
- **Tray:** Switch accounts from an icon in the Windows notification area or the macOS menu bar.
- **Install:** Install the CLI and a background agent that runs at login on Windows and macOS, with `install` and `uninstall`. Release archives are available for Windows x64, Windows Arm64, and macOS on Apple silicon.
- **Plugin:** Use the tool from Claude Code through the `account-sync` plugin and its skill.
