# claude-account-sync

> [!WARNING]
> This tool is unofficial. It depends on the app's undocumented file layout, and an app update can break it.

Switches the Claude desktop app between accounts without signing out, and shares the Code tab's sessions between those accounts. It runs on Windows and macOS.

The app shows Code tab sessions, the task backlog, and local scheduled tasks per account, although the transcripts under `~/.claude` belong to no account. This tool merges the per-account lists so that every account shows the same sessions. It also saves each account's sign-in, so that you can switch accounts from a menu instead of signing out and in again.

> [!TIP]
>
> To install it, give this prompt to a coding agent such as Claude Code:
>
> ```text
> Install claude-account-sync on this computer from https://github.com/SegaraRai/claude-account-sync,
> following the "Install" section of its README.md. Use the latest GitHub Release for this operating
> system and CPU, and verify the archive before you run anything from it. Ask me before you run
> `claude-account-sync install`, because it registers a program that starts at login. When it is
> done, show me `claude-account-sync status` and `claude-account-sync profiles`, and tell me how to
> add my other accounts.
> ```

## Features

- **Account switching:** an icon in the notification area (the menu bar on macOS) lists your accounts with their names, email addresses, and last use, and switches the app to the one you choose. The tool quits the app, restores that account's saved sign-in, and starts the app again.
- **Adding accounts without signing out:** the tool sets the current sign-in aside and opens the app's sign-in page for another account.
- **Session sync:** the sidebar sessions, the task backlog, and local scheduled tasks of every account stay the same, including archiving and deletion.
- **Sign-out recovery:** when the app signs out, a picker offers the other saved accounts.
- **Safety:** the tool backs up every file before it replaces it, never decrypts sign-ins, and asks before it stops running Claude Code sessions.

The tool does not sync claude.ai chats, Projects, Artifacts, Cowork sessions, connectors, or data on another computer.

## Requirements

- Windows on x64 or Arm64, or macOS on Apple silicon.
- The Claude desktop app, from claude.ai or from the Microsoft Store.

## Install

### With an agent

Give the prompt at the top of this file to a coding agent. The agent downloads the archive for your platform, verifies it, and runs `install` after you agree.

For an agent that should manage the tool later, you can also add the Claude Code plugin. Its skill tells Claude how to check the sync, change settings, and switch accounts:

```sh
claude plugin marketplace add SegaraRai/claude-account-sync
claude plugin install account-sync@account-sync
```

### Manually

1. Download the archive for your platform from [Releases](https://github.com/SegaraRai/claude-account-sync/releases): `claude-account-sync-windows-x64.zip`, `claude-account-sync-windows-arm64.zip`, or `claude-account-sync-macos-arm64.zip`.
2. Optionally, verify it against `SHA256SUMS` from the same release, or with `gh attestation verify <archive> --repo SegaraRai/claude-account-sync`.
3. Extract it, and run `install` from the extracted folder:

   ```sh
   ./claude-account-sync install
   ```

   `install` copies both executables into `bin/` in the state folder (see [Files](#files)), registers the agent to start at login, and starts it. You can delete the extracted folder afterwards. To update, run `install` from a newer release.

   The macOS executables are not notarized. A browser marks downloaded files as quarantined, and Gatekeeper then refuses to run them. Download with `curl` or `gh release download`, or remove the mark first with `xattr -dr com.apple.quarantine <extracted folder>`. `install` removes it from the installed copies.

To build from source instead, install Rust, run `cargo build --release`, and run `target/release/claude-account-sync install`.

### First steps

After `install`, the agent runs in the background and shows an account icon in the notification area on Windows, or in the menu bar on macOS. It saves the signed-in account's sign-in whenever the app is stopped.

1. To add another account, choose **Sign in to another account…** in the account icon's menu. The tool saves the current account's sign-in first, then the app shows its sign-in page. Sign in to the other account there.
2. From then on, choose an account in the menu to switch to it.

Do not sign out in the app to switch or add accounts. Signing out most likely revokes that account's saved sign-in.

## Switching accounts

The account icon's menu lists the saved accounts with the time each was last used, checks the signed-in one, and greys out the ones that need a new sign-in. **Choose account…** opens the picker window, which also removes accounts. **Open log** opens the agent's log, and **Quit claude-account-sync** stops the icon and the sync until the next login.

The same works from the command line:

```sh
claude-account-sync profiles                     # saved accounts, most recently used first
claude-account-sync switch                       # pick an account in the terminal
claude-account-sync switch --gui                 # pick an account in a window
claude-account-sync switch work                  # by name, email address, label, or UUID
claude-account-sync switch work --dry-run        # ask the questions and show the plan only
claude-account-sync switch --new                 # open the sign-in page for another account
claude-account-sync profiles label <account> Work
claude-account-sync profiles forget <account>
```

When you switch:

- If Claude Code sessions in the app are in the middle of a turn or have background tasks running, such as commands, monitors, or agents, the tool lists them and asks first, because quitting the app stops them. You can continue them after the switch.
- The tool asks the app to quit, so that the app saves its data. If the app does not quit within 60 seconds, a window asks you to quit it, and the switch continues when it has.
- The app starts again with the chosen account. The files that the switch replaced are kept in `backups/<time>/sign-in/`.

**Adding an account.** **Sign in to another account…** in the menu or the picker, or `switch --new`, saves the current sign-in, removes it from the app without signing out, and starts the app on its sign-in page. The agent saves the new account the next time the app stops, and the previous account stays usable.

**Removing an account.** Select it in the picker and choose **Remove…** (on macOS, choose **Remove an account…** at the end of the list), enter `r` and its number in the terminal picker, or run `profiles forget`. This deletes the saved sign-in, name, and last-used time from the tool, and does not affect the account or its sessions. The signed-in account cannot be removed.

**Names.** The agent keeps each account's name and email address up to date from the app, so an account shows its name once it has been signed in while the agent runs. Until then it shows as `account <first UUID block>`. `profiles label` sets a name that takes precedence.

**When the app signs out.** The agent marks that account's saved sign-in as unusable and opens the picker, so that you can continue with another saved account. The signed-out account needs one normal sign-in before you can switch to it again.

## Syncing

| Data                  | What stays the same in every account                     |
| --------------------- | -------------------------------------------------------- |
| Sidebar sessions      | The session list, titles, archiving, and deletion        |
| Task backlog          | The backlog items                                        |
| Local scheduled tasks | The tasks; each account keeps its own record of the runs |

The agent syncs after every change and every minute. When several accounts changed the same entry, the newest version wins, and a deletion wins over a concurrent edit. A file that the tool cannot validate is never copied or overwritten.

While the app runs, it holds the signed-in account's data in memory, so the tool does not write to that account. Changes for it arrive when the app quits or you switch accounts.

The tool copies every file that it replaces or removes to `backups/<time>/` in the state folder, and the first sync copies the whole session folder to `backups/<time>/initial/`. It keeps the newest 50 backup folders, plus the one with `initial/`. `status` shows the backup folder and its size.

[docs/how-it-works.md](docs/how-it-works.md) describes the merge rules and the files in detail.

## Commands

| Command                              | What it does                                                                       |
| ------------------------------------ | ---------------------------------------------------------------------------------- |
| `status`                             | Shows the app's state, the account folders, and the changes that a sync would make |
| `accounts`                           | Lists the account and organization folders with their session counts               |
| `sync [--dry-run]`                   | Runs one sync pass                                                                 |
| `watch`                              | Syncs after every change until stopped, like the agent but without the icon        |
| `install`                            | Installs or updates the executables and the agent, and starts the agent            |
| `uninstall`                          | Stops the agent and removes its login registration                                 |
| `profiles [list]`                    | Lists the saved accounts                                                           |
| `profiles save`                      | Saves the current sign-in now; the app must be stopped                             |
| `profiles label <account> [<label>]` | Names an account, or removes its name                                              |
| `profiles forget <account>`          | Removes a saved account                                                            |
| `switch [<account>]`                 | Switches to an account; see `switch --help` for the options                        |
| `tray [--dry-run]`                   | Shows the account icon without the sync, to try the menu                           |
| `config ...`                         | Shows or changes the settings                                                      |

Every command accepts `--json` and then prints one JSON document. `sync` exits with code 2 when the pass finished but skipped entries or folders. `--data-dir` overrides the app's data folder, and `--state-dir` overrides the state folder.

## Configuration

`config.json` in the state folder holds the settings. Change them through the CLI, which validates every value:

```sh
claude-account-sync config show
claude-account-sync config set <key> <value>
claude-account-sync config reset <key>
claude-account-sync config exclude <account>[/<org>]
claude-account-sync config include <account>[/<org>]
```

| Key                   | Default           | Effect                                                  |
| --------------------- | ----------------- | ------------------------------------------------------- |
| `sessions`            | `true`            | Sync sidebar sessions                                   |
| `backlog`             | `true`            | Sync the task backlog                                   |
| `scheduled-tasks`     | `true`            | Sync local scheduled tasks                              |
| `watch-interval-secs` | `60`              | Seconds between syncs when nothing changes (5 to 86400) |
| `backups-to-keep`     | `50`              | Backup folders to keep (1 to 10000)                     |
| `data-dir`            | the app's default | The app's data folder, when the tool cannot find it     |
| `save-sign-ins`       | `true`            | Save each account's sign-in and record its last use     |
| `picker-on-sign-out`  | `true`            | Open the picker when the app signs out                  |
| `tray-icon`           | `true`            | Show the account icon                                   |

`config exclude` stops syncing an account, or one of its organizations. An excluded folder is neither read nor written. When you include it again, it receives the other accounts' entries, and entries that were deleted elsewhere in the meantime are removed from it.

The agent reads the settings before each sync. Changes to `data-dir` and `tray-icon` take effect after `install` restarts the agent. An unknown field or an invalid value in `config.json` stops every sync with an error until the file is fixed.

## Files

The tool keeps its settings, state, saved sign-ins, backups, installed executables, and the agent's log (`agent.log`) in its state folder:

- Windows: `%USERPROFILE%\.claude-account-sync`
- macOS: `~/Library/Application Support/claude-account-sync`

The agent starts at login as the Task Scheduler task `claude-account-sync` on Windows, or as the launchd agent `io.github.claude-account-sync` on macOS.

Saved sign-ins stay encrypted with the key that the operating system protects for your user, as the app's own copy is. The tool copies the encrypted bytes and never decrypts them, so a saved sign-in works only for the same user on the same computer.

## Uninstall

```sh
claude-account-sync uninstall
```

This stops the agent and removes its login registration. It keeps the state folder, including the backups and the saved sign-ins. Delete the state folder to remove them too.

## More

- [docs/how-it-works.md](docs/how-it-works.md): the merge rules, the saved sign-ins, and what a switch does in detail.
- [docs/development.md](docs/development.md): building, checking, the Claude Code plugin, and the assets.
- [docs/releasing.md](docs/releasing.md): changelog entries and releases.
- [CHANGELOG.md](CHANGELOG.md): the changes in each release.
