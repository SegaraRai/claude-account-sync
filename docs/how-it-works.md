# How claude-account-sync works

This document describes what the tool reads and writes, for users who want the details and for contributors. [README.md](../README.md) covers how to use it.

## Where the app keeps its data

The Claude desktop app keeps transcripts, settings, and memory under `~/.claude`, and these do not depend on the account. The sidebar session list, the task backlog, and local scheduled tasks are stored per account and organization in `claude-code-sessions/<account>/<org>/` in the app's data folder. The tool calls each of these folders a replica.

| Data                  | File in each replica                       | Merge unit                                        |
| --------------------- | ------------------------------------------ | ------------------------------------------------- |
| Sidebar sessions      | `local_<id>.json`, `archived-sessions.idx` | One record file per session, copied byte for byte |
| Task backlog          | `backlog/tasks.json`                       | One item per `id`                                 |
| Local scheduled tasks | `scheduled-tasks.json`                     | One entry per `id` in `scheduledTasks`            |

For scheduled tasks, fields outside `scheduledTasks`, such as `recordedSkips` and `runRetries`, record runs in one account. Each account keeps its own values for them.

## Merging

Each pass compares every replica with the state that the previous pass left, which is stored in `state.json`:

- An entry that changed in one replica is copied to the other replicas.
- When several replicas changed the same entry, the newest version wins. Sessions are ordered by `lastActivityAt`, backlog items by `filedAt`, and scheduled tasks by `lastRunAt`.
- An entry that the user deleted in one replica is removed from the other replicas. A session counts as deleted when its record disappears or when the app writes a `deleted_<id>` marker. Deletion wins over a concurrent edit.
- A file that fails schema validation is never copied and never overwritten.
- After the tool changes session records in a replica, it rewrites `archived-sessions.idx` from the records' `isArchived` fields.

While the app runs, it keeps the active account's data in memory and writes that data back later. Therefore the tool writes only to inactive replicas while the app runs. Changes for the active account wait until the app quits or the user switches to another account.

An excluded replica is neither read nor written. When it is included again, it receives the other replicas' entries, and entries that were deleted elsewhere in the last 90 days are removed from it. A disabled collection keeps its state and continues from it when it is enabled again.

### Backups

Before the tool replaces or removes a file, it copies the file to `backups/<time>/` in the state folder. Each backup folder mirrors `claude-code-sessions/<account>/<org>/`. The first pass also copies the whole `claude-code-sessions` folder to `backups/<time>/initial/`. The tool keeps the newest `backupsToKeep` backup folders (50 by default), plus the folder that holds `initial/`.

## The watcher

`claude-account-sync-agent` runs `watch` in the background. It runs a pass after every change in the app's data folder and every `watchIntervalSecs` seconds, which catches changes that produce no file event, such as the app starting or stopping. It reads `config.json` before each pass. After each pass it also does the account work below.

## Saved sign-ins

The app keeps its sign-in in its cookie database (`Network/Cookies`) and in the `oauth:tokenCache` and `oauth:tokenCacheV2` fields of its `config.json`. Both are encrypted with a key that the operating system protects for the current user. The tool never decrypts them. It copies the encrypted bytes into `profiles/<account>/` in the state folder and back, so a saved sign-in works only for the same user on the same computer. Anyone who can run programs as that user can use it, as with the app's own copy.

The watcher saves the sign-in of the signed-in account whenever the app is stopped, because the app locks its cookie database while it runs. It also records when each account was last used.

### Names and email addresses

The app's claude.ai page caches the signed-in account, including its name and email address, in IndexedDB (`IndexedDB/https_claude.ai_0.indexeddb.leveldb`). After every pass the watcher scans the newest records of that database and copies the values into the matching profile, replacing older ones. The Claude Code CLI's profile cache in `oauthAccount` of `~/.claude.json` (`$CLAUDE_CONFIG_DIR/.claude.json` when that is set) fills in values that the app's cache lacks, such as the organization. Neither cache creates a profile, so a removed account does not come back.

### Sign-out detection

Signing out in the app most likely revokes that session on the server, and the tool assumes it does. When the running app shows no sign-in for 5 seconds after it was signed in, the watcher marks that account's saved sign-in as unusable and, unless `pickerOnSignOut` is off, opens the account picker. The account needs one normal sign-in before it can be restored again.

`switch --new` also removes the sign-in from the app, but without signing out. The watcher tells the two apart by the time: when the account's sign-in was saved after the watcher last saw it signed in, a switch moved it, and the saved sign-in stays usable.

## Switching

`switch` does this:

1. If Claude Code sessions of the app are working, it lists them and asks before it continues, because quitting the app stops them (see [Running sessions](#running-sessions)).
2. It asks the app to quit normally, so that the app saves its data first. On macOS it sends the quit Apple event. On Windows, closing the window only hides the app in the notification area, so the tool sends the app's main window the messages that Windows sends when it closes applications at sign-out (`WM_QUERYENDSESSION` and `WM_ENDSESSION` with `ENDSESSION_CLOSEAPP`). The app answers them with the same quit as its tray menu's Quit. If the app is still running after 60 seconds, a window asks the user to quit it, and the switch continues when it has exited, for up to `--quit-timeout-secs`. A signed-out app holds no account data, so the tool kills it.
3. It saves the current sign-in, so that switching back restores it.
4. It restores the chosen account's sign-in, or for `--new` removes the sign-in (the OAuth fields, `lastKnownAccountUuid`, and the cookie database). It copies the files that it replaces to `backups/<time>/sign-in/`. Other fields of `config.json` stay as they are.
5. It starts the app again.

## Running sessions

Each running Claude Code process registers itself in `sessions/<pid>.json` in the CLI's config folder (`~/.claude`, or `$CLAUDE_CONFIG_DIR`). The file names the session, the entry point (`claude-desktop` for the app's sessions), and the status: `busy` while a turn runs, `idle` otherwise. The tool uses the files of the app's sessions whose process still runs and started before it registered, which rules out a reused process ID.

The status stays `idle` while background tasks run after a turn. For idle sessions, the tool reads the transcript (`projects/<project>/<session>.jsonl`): the tool result that starts a background command, monitor, or agent records its task ID (`backgroundTaskId`, `taskId`, or `agentId` with `isAsync`), and the CLI records a `<task-notification>` with the same ID when the task ends. A task that started after the process started and has no notification yet still runs. Tasks from before the process started ended with the previous process.

A session counts as working when it is busy or has such a task. Without the `sessions` folder, as with older CLI versions, the tool falls back to sessions whose transcript changed in the last 5 minutes, which can include idle ones.

## Windows and the Microsoft Store app

On Windows, the state folder (`%USERPROFILE%\.claude-account-sync`) and the login registration (a Task Scheduler task) avoid `AppData` and the `Run` registry key. The Microsoft Store version of the Claude desktop app runs Claude Code inside its MSIX package. Windows redirects a packaged process's writes to new files under `AppData` and to `HKCU` into private package storage, so programs outside the package and Windows at login would not see them when Claude Code runs `install`.
