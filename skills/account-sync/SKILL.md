---
name: account-sync
description: Share Claude desktop Code tab sessions, the task backlog, and local scheduled tasks between Claude accounts on the same computer with the claude-account-sync CLI. Use when the user wants sessions from another account to appear in the sidebar, asks whether account sync works, wants to change what is synced or which accounts take part, or wants to install, update, pause, or remove the sync watcher.
---

# claude-account-sync

The Claude desktop app shows Code tab sessions per account, but transcripts under `~/.claude/projects` do not depend on the account. `claude-account-sync` merges the per-account folders `claude-code-sessions/<account>/<org>/` so that every account shows the same sessions. It also merges `backlog/tasks.json` and `scheduled-tasks.json`.

It does not sync claude.ai chats, Projects, Artifacts, Cowork sessions, connectors, or data on other computers. Do not promise those.

## Find the CLI

Use the first one that exists:

1. The installed copy:
   - Windows: `%LOCALAPPDATA%\claude-account-sync\bin\claude-account-sync.exe`
   - macOS: `~/Library/Application Support/claude-account-sync/bin/claude-account-sync`
2. `claude-account-sync` on `PATH`.
3. A build from this plugin's source. The plugin folder is the tool's repository, and the build needs Rust (`cargo`). Build into the plugin data folder, because the plugin folder is replaced on each plugin update:

   ```sh
   cargo build --release --manifest-path "${CLAUDE_PLUGIN_ROOT}/Cargo.toml" --target-dir "${CLAUDE_PLUGIN_DATA}/target"
   ```

   Then use `${CLAUDE_PLUGIN_DATA}/target/release/claude-account-sync`. Run `install` from this build to place a lasting copy in the installed location.

After a plugin update, build again and run `install` again, so that the installed copy matches this skill.

Pass `--json` to every command whose output you read. Plain output is for people.

## Check the state first

```sh
claude-account-sync status --json
```

The result contains:

- `app.running` and `app.activeAccount`: while the app runs, the tool never writes the active account, and that replica has `"writable": false`. This is expected, not an error. Changes for that account wait until the user quits the app or switches accounts.
- `report.changes`: the changes that a sync would make now. Each has `collection`, `key`, `target`, `kind` (`copy` or `remove`), and `from` for copies.
- `report.problems`: entries or replicas that were skipped. An invalid file is never copied or overwritten. Report problems to the user instead of editing app files to fix them.
- `report.excluded` and `report.disabledCollections`: what the config leaves out.
- `backups`: the backup folder (`dir`), its size (`totalBytes`), and each backup folder, oldest first. A folder with `"initial": true` holds the data from before the first sync. Pruning never removes it. Tell the user about it before they delete backups by hand.

`claude-account-sync accounts --json` lists each replica with its session count, whether it is active, writable, and excluded.

## Sync once

Show the user the planned changes from `status --json`, especially any `remove` changes, and get agreement before you run:

```sh
claude-account-sync sync --json
```

Exit code 2 means that the pass finished with problems. `report.backupDir` names the folder that holds the replaced or removed files.

## Change settings

Settings live in `config.json` in the state folder. Change them only through the CLI, which validates every value. Do not edit the file by hand.

```sh
claude-account-sync config show --json
claude-account-sync config set <key> <value>
claude-account-sync config reset <key>
claude-account-sync config exclude <account-uuid>[/<org-uuid>]
claude-account-sync config include <account-uuid>[/<org-uuid>]
```

| Key | Value | Effect |
|---|---|---|
| `sessions` | `true` / `false` | Sync sidebar sessions |
| `backlog` | `true` / `false` | Sync the task backlog |
| `scheduled-tasks` | `true` / `false` | Sync local scheduled tasks |
| `watch-interval-secs` | 5 to 86400 | Seconds between watcher passes without file changes |
| `backups-to-keep` | 1 to 10000 | Backup folders to keep |
| `data-dir` | absolute path | The app's data folder, when the tool cannot find it |

An excluded account or replica is neither read nor written. When it is included again, it receives the other accounts' entries, and entries that were deleted elsewhere meanwhile are removed from it. Tell the user this before you include an account again.

The watcher reads the config before each pass. A `data-dir` change needs `install` again to restart the watcher.

## Install, update, or remove the watcher

`install` registers a login item (Windows `Run` registry key, or a macOS launchd agent) and starts it. This changes the user's startup configuration, so ask the user before you run it. On macOS, `install` also removes the quarantine attribute from the installed copies, and `installed.clearedQuarantine` lists them. Mention this to the user when the list is not empty.

```sh
claude-account-sync install     # also updates the installed copy after a rebuild
claude-account-sync uninstall   # stops the watcher; keeps state, backups, and logs
```

To pause syncing without removing the watcher, disable all three collections with `config set`.

The watcher log is `agent.log` in the state folder: `%LOCALAPPDATA%\claude-account-sync` on Windows, `~/Library/Application Support/claude-account-sync` on macOS.

## Restore from a backup

Each folder under `backups/` in the state folder mirrors `claude-code-sessions/<account>/<org>/`. `backups/<time>/initial/` holds the whole sessions folder from before the first sync. To restore, ask the user to quit the app, then copy the files back. If the tool would delete a restored session again, the restored session is in the state's deleted list. Tell the user, and do not edit `state.json` without the user's agreement.
