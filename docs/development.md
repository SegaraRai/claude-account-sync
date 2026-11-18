# Development

## Build and check

The tool is a Rust crate with two executables: `claude-account-sync` (the CLI) and `claude-account-sync-agent` (the background watcher with the tray icon).

```sh
cargo build --release
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo check --target aarch64-apple-darwin --all-targets   # the macOS code paths, from Windows
claude plugin validate .
```

`claude-account-sync tray --dry-run` shows the tray icon and starts every switch from its menu with `--dry-run`, and `switch --gui --dry-run` shows the picker windows, so that you can try both without changing anything. A **Remove…** that you confirm in the picker still removes the account.

## CI and releases

The CI workflow builds, lints, and tests Windows x64, Windows Arm64, and macOS on Apple silicon on every push and pull request, and keeps the archives as workflow artifacts for 14 days. Pushing a tag `v<version>` runs the Release workflow, which creates a draft GitHub Release. [releasing.md](releasing.md) describes changelog entries and the release steps.

## The Claude Code plugin

This repository is also a Claude Code plugin marketplace. Its plugin `account-sync` contains the skill [`skills/account-sync/SKILL.md`](../skills/account-sync/SKILL.md), which tells Claude how to find or build the CLI, run it, configure it, and install it. The plugin name does not start with `claude-` because Claude Code reserves that prefix.

Keep `version` in [`.claude-plugin/plugin.json`](../.claude-plugin/plugin.json) equal to the crate version. Users receive a plugin update only when this version changes. Update the skill when a command, option, or setting changes.

## Assets

`assets/` holds the icon: `icon.svg`, PNG renderings from 16 to 1024 pixels in `assets/png/`, and `icon.ico`. `tray-template.svg` and its PNGs are the monochrome menu bar version, which macOS tints. The executables embed `icon-32.png` (Windows) and `tray-template-44.png` (macOS) for the tray.
