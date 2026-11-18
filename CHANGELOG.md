# Changelog

This file lists the changes that users notice in each release. [docs/releasing.md](docs/releasing.md) describes how to add entries and cut a release.

## Unreleased

First release.

### Features

- **Sync:** Merge the Code tab's sidebar sessions, task backlog, and local scheduled tasks between all accounts that sign in to the Claude desktop app on the same computer. Edits and deletions in one account reach the others, and replaced files are backed up first.
- **Switching:** Save each account's sign-in and switch the app between accounts without signing out, with `switch [<account>]`. The tool quits the app, restores the sign-in, and starts the app again, and asks first when Code tab sessions are running.
- **Switching:** Add another account without signing out of the current one, with `switch --new`.
- **Switching:** Show an account picker with each account's name, email, and last use when the user signs out of the app.
- **Tray:** Switch accounts from an icon in the Windows notification area or the macOS menu bar.
- **Install:** Install the CLI and a background agent that runs at login on Windows and macOS, with `install` and `uninstall`.
- **Plugin:** Use the tool from Claude Code through the `account-sync` plugin and its skill.
