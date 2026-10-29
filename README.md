# claude-account-sync

Shares the Claude desktop app's Code tab data between accounts that sign in on the same computer. It runs on Windows and macOS.

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
```

Every command accepts `--json` and then prints one JSON document. `sync` exits with code 2 when the pass finished but skipped entries or replicas.

`install` copies `claude-account-sync` and `claude-account-sync-agent` into the state folder's `bin/`. It registers the agent as the Task Scheduler task `claude-account-sync` with a logon trigger on Windows, or as the launchd agent `io.github.claude-account-sync` on macOS. The agent opens no window and writes its log to `agent.log`.

On macOS, `install` also removes the `com.apple.quarantine` attribute that a browser adds to downloaded files, so that Gatekeeper does not block the copied agent at login. `install` prints each file whose attribute it removed. The executables still need a code signature: Cargo signs arm64 builds ad hoc, and a distributed build should be signed and notarized.

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
  "dataDir": "C:\\Users\\me\\AppData\\Roaming\\Claude"
}
```

Change settings through the CLI, which applies the same validation:

```sh
claude-account-sync config show
claude-account-sync config set scheduled-tasks false   # keys: sessions, backlog, scheduled-tasks,
claude-account-sync config reset scheduled-tasks       #   watch-interval-secs, backups-to-keep, data-dir
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
