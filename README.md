# claude-account-sync

Shares the Claude desktop app's Code tab data between accounts that sign in on the same computer, and switches the app between those accounts without signing out. It runs on Windows and macOS.

The app keeps transcripts, settings, and memory under `~/.claude`, and these do not depend on the account. The sidebar session list, the task backlog, and local scheduled tasks are stored per account and organization in `claude-code-sessions/<account>/<org>/`. This tool merges those folders so that every account shows the same sessions.

This tool is unofficial. It depends on the app's undocumented file layout, and an app update can break it.

## What it syncs

| Data                  | File in each `<account>/<org>` folder      | Merge unit                                        |
| --------------------- | ------------------------------------------ | ------------------------------------------------- |
| Sidebar sessions      | `local_<id>.json`, `archived-sessions.idx` | One record file per session, copied byte for byte |
| Task backlog          | `backlog/tasks.json`                       | One item per `id`                                 |
| Local scheduled tasks | `scheduled-tasks.json`                     | One entry per `id` in `scheduledTasks`            |

The tool does not sync claude.ai chats, Projects, Artifacts, Cowork sessions, connectors, or data on another computer.

For scheduled tasks, fields outside `scheduledTasks`, such as `recordedSkips` and `runRetries`, record runs in one account. Each account keeps its own values for them.

## How the merge works

Each pass compares every folder with the state that the previous pass left, which is stored in `state.json`:

- An entry that changed in one folder is copied to the other folders.
- When several folders changed the same entry, the newest version wins. Sessions are ordered by `lastActivityAt`, backlog items by `filedAt`, and scheduled tasks by `lastRunAt`.
- An entry that the user deleted in one folder is removed from the other folders. A session counts as deleted when its record disappears or when the app writes a `deleted_<id>` marker. Deletion wins over a concurrent edit.
- A file that fails schema validation is never copied and never overwritten.
- After the tool changes session records in a folder, it rewrites `archived-sessions.idx` from the records' `isArchived` fields.

While the app runs, it keeps the active account's data in memory and writes that data back later. Therefore the tool writes only to inactive accounts while the app runs. Changes for the active account wait until the app quits or the user switches to another account.

Before the tool replaces or removes a file, it copies the file to `backups/<time>/`. The first pass also copies the whole `claude-code-sessions` folder to `backups/<time>/initial/`. The tool keeps the newest 50 backup folders, plus the folder that holds `initial/`. `status` shows the backup folder and its size.

## Switching accounts

Signing out of the app and signing in to another account takes several steps. The tool can save each account's sign-in and restore it instead.

The easiest way is the account icon that the watcher shows in the notification area on Windows and in the menu bar on macOS. Its menu lists the saved accounts with the time each was last used, checks the signed-in one, and greys out the ones that need a new sign-in. Choose an account to switch to it, or choose **Sign in to another account…** or **Choose account…**. **Quit claude-account-sync** stops the icon and the watcher until the next login. `config set tray-icon false` hides the icon and keeps the watcher, after `install` restarts the agent.

The same works from the command line:

```sh
claude-account-sync profiles                 # saved accounts, most recently used first
claude-account-sync profiles label <account> Work
claude-account-sync switch                   # pick an account in the terminal
claude-account-sync switch --gui             # pick an account in a window
claude-account-sync switch work              # by label, UUID, or the start of a UUID
claude-account-sync switch work --dry-run    # ask the questions and show the plan only
claude-account-sync switch --new             # open the sign-in page for another account
claude-account-sync profiles forget <account>
```

The app keeps its sign-in in its cookie database (`Network/Cookies`) and in the `oauth:tokenCache` and `oauth:tokenCacheV2` fields of its `config.json`. Both are encrypted with a key that the operating system protects for the current user. The tool never decrypts them. It copies the encrypted bytes into `profiles/<account>/` in the state folder and back, so a saved sign-in works only for the same user on the same computer. Anyone who can run programs as that user can use it, as with the app's own copy.

The watcher saves the sign-in of the signed-in account whenever the app is stopped, and it records when each account was last used.

The watcher keeps each account's name and email address up to date after every pass. It reads them from the app's own cache of the signed-in account, in the claude.ai page's IndexedDB (`IndexedDB/https_claude.ai_0.indexeddb.leveldb`), so an account shows its current name once it has been signed in while the watcher runs. The Claude Code CLI's profile cache in `oauthAccount` of `~/.claude.json` (`$CLAUDE_CONFIG_DIR/.claude.json` when that is set) fills in values that the app's cache lacks, such as the organization. An account whose name is not known yet shows as `account <first UUID block>`. `profiles label` sets a name that takes precedence.

To remove a saved account, select it in the picker and choose **Remove…** (on macOS, choose **Remove an account…** at the end of the list), or enter `r` and its number in the terminal picker, or run `profiles forget`. This deletes the saved sign-in, name, and last-used time from the tool, and does not affect the account or its sessions. The signed-in account cannot be removed, because the watcher would save it again.

To add an account, choose **+ Sign in to another account** in the picker, or run `switch --new`. The tool saves the current sign-in, removes it from the app without signing out on the server (it deletes the OAuth fields, `lastKnownAccountUuid`, and the cookie database, after a backup), and starts the app, which then shows its sign-in page. Sign in to the other account there. The watcher saves that sign-in the next time the app stops, and the previous account stays usable.

`switch` does this:

1. If Claude Code sessions of the app wrote their transcript in the last 5 minutes, it lists them and asks before it continues, because quitting the app stops them.
2. It asks the app to quit normally, so that the app saves its data first. On macOS it sends the quit Apple event. On Windows, closing the window only hides the app in the notification area, so the tool sends the app's main window the messages that Windows sends when it closes applications at sign-out (`WM_QUERYENDSESSION` and `WM_ENDSESSION` with `ENDSESSION_CLOSEAPP`). The app answers them with the same quit as the tray menu's Quit. If the app is still running after 60 seconds, a window asks you to quit it yourself, and the switch continues when it has exited. A signed-out app holds no account data, so the tool kills it.
3. It saves the current sign-in, so that switching back restores it.
4. It restores the chosen account's sign-in, or for `--new` removes the sign-in, and copies the files that it replaces to `backups/<time>/sign-in/`. Other fields of `config.json` stay as they are.
5. It starts the app again.

Signing out in the app most likely revokes that session on the server, and this design assumes it does. Do not sign out to switch or add accounts. When the watcher sees the running app sign out, it marks that account's saved sign-in as unusable and opens the account picker, so that you can continue with another saved account. The signed-out account needs one normal sign-in before it can be restored again.

`config set save-sign-ins false` stops saving sign-ins and recording use. `config set picker-on-sign-out false` keeps the picker closed after a sign-out.

## Usage

```sh
cargo build --release
claude-account-sync status          # show the planned changes without making them
claude-account-sync accounts        # list account folders with session counts
claude-account-sync sync --dry-run  # same plan, without the status header
claude-account-sync sync            # run one pass
claude-account-sync watch           # sync after every change until stopped
claude-account-sync install         # run the watcher at login, start it now, or update it
claude-account-sync uninstall       # stop the watcher and remove the login registration
claude-account-sync tray            # show the account icon without the watcher (add --dry-run to try the menu)
```

Every command accepts `--json` and then prints one JSON document. `sync` exits with code 2 when the pass finished but skipped entries or replicas.

`install` copies `claude-account-sync` and `claude-account-sync-agent` into the state folder's `bin/`. It registers the agent as the Task Scheduler task `claude-account-sync` with a logon trigger on Windows, or as the launchd agent `io.github.claude-account-sync` on macOS. The agent opens no window and writes its log to `agent.log`.

On macOS, `install` also removes the `com.apple.quarantine` attribute that a browser adds to downloaded files, so that Gatekeeper does not block the copied agent at login. `install` prints each file whose attribute it removed. The executables still need a code signature: Cargo signs arm64 builds ad hoc, and a distributed build should be signed and notarized.

`assets/` holds the icon: `icon.svg`, PNG renderings from 16 to 1024 pixels in `assets/png/`, and `icon.ico`. `tray-template.svg` and its PNGs are the monochrome menu bar version, which macOS tints. The executables embed `icon-32.png` (Windows) and `tray-template-44.png` (macOS) for the tray.

The tool stores its config, state, backups, and logs in these folders:

- Windows: `%USERPROFILE%\.claude-account-sync`
- macOS: `~/Library/Application Support/claude-account-sync`

On Windows, the folder and the login registration avoid `AppData` and the `Run` registry key. The Microsoft Store version of the Claude desktop app runs Claude Code inside its MSIX package. Windows redirects a packaged process's writes to new files under `AppData` and to `HKCU` into private package storage, so programs outside the package and Windows at login do not see them.

`--data-dir` and `--state-dir` override the app's data folder and this folder.

## Configuration

`config.json` in the state folder holds the settings. The file and each field are optional, and missing ones take the defaults. An unknown field or an invalid value stops every pass with an error, until the file is fixed.

```json
{
  "version": 1,
  "collections": {
    "sessions": true,
    "backlog": true,
    "scheduledTasks": true
  },
  "excluded": ["<account UUID>", "<account UUID>/<org UUID>"],
  "watchIntervalSecs": 60,
  "backupsToKeep": 50,
  "dataDir": "C:\\Users\\me\\AppData\\Roaming\\Claude",
  "saveSignIns": true,
  "pickerOnSignOut": true,
  "trayIcon": true
}
```

Change settings through the CLI, which applies the same validation:

```sh
claude-account-sync config show
claude-account-sync config set scheduled-tasks false   # keys: sessions, backlog, scheduled-tasks,
claude-account-sync config reset scheduled-tasks       #   watch-interval-secs, backups-to-keep, data-dir,
                                                       #   save-sign-ins, picker-on-sign-out, tray-icon
claude-account-sync config exclude <account>[/<org>]
claude-account-sync config include <account>[/<org>]
```

An excluded replica is neither read nor written. When it is included again, it receives the other replicas' entries, and entries that were deleted elsewhere in the last 90 days are removed from it. A disabled collection keeps its state and continues from it when it is enabled again.

The watcher reads the config before each pass. A change to `dataDir` takes effect after `install` restarts the watcher.

## Use from Claude Code

This repository is also a Claude Code plugin marketplace. Its plugin `account-sync` contains the skill [`skills/account-sync/SKILL.md`](skills/account-sync/SKILL.md), which tells Claude how to build, run, configure, and install the tool. The plugin name does not start with `claude-` because Claude Code reserves that prefix.

```sh
claude plugin marketplace add <owner>/<repo>   # or a local path to this repository
claude plugin install account-sync@account-sync
```

When the CLI is not installed yet, the skill builds it from the plugin's copy of this repository into the plugin data folder, which needs Rust. It then runs `install`. After a plugin update, Claude builds the CLI and runs `install` again, so that the installed CLI matches the skill.

Keep `version` in [`.claude-plugin/plugin.json`](.claude-plugin/plugin.json) equal to the crate version. Users receive a plugin update only when this version changes.

## Verification

```sh
cargo test
cargo clippy --all-targets
cargo check --target aarch64-apple-darwin --all-targets
claude plugin validate .
```
