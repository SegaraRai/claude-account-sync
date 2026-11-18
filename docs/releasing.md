# Releasing

This document is for coding agents and maintainers. It covers the changelog entries that come with each change and the steps that publish a release.

## Changelog entries

Every change that users notice adds an entry under `## Unreleased` in [CHANGELOG.md](../CHANGELOG.md), in the same commit as the change. Users notice behavior, commands and their options, settings, files that the tool reads or writes, supported platforms, and installation. Refactoring, tests, CI, and documentation changes get no entry unless they change what users do.

Put each entry under one of these headings, in this order, and omit empty headings:

1. `### Breaking changes`: users must act after the update, such as renaming a setting or running `install` again. State the action in the entry.
2. `### Features`
3. `### Fixes`

Start each entry with a scope label, and order the entries in each heading by label:

1. `Sync`: merging sessions, the backlog, and scheduled tasks.
2. `Switching`: saved sign-ins, `switch`, the picker, and sign-out handling.
3. `Tray`: the notification area icon and the macOS menu bar item.
4. `CLI`: commands, options, output, and settings that no label above covers.
5. `Install`: `install`, `uninstall`, the agent, and login registration.
6. `Plugin`: the Claude Code plugin and its skill.

Write for users, in the style of README.md: plain, specific sentences without marketing words. When a later change in the same release modifies an unreleased entry, edit that entry instead of adding another one.

## Cutting a release

Release only when the maintainer asks, and confirm the version with them. Follow [semantic versioning](https://semver.org/); while the version is below 1.0.0, a breaking change raises the minor version. A version with a suffix, such as `0.2.0-rc.1`, becomes a prerelease.

1. Start from an up-to-date `main` with a clean working tree, and check that the CI workflow passed on its latest commit.
2. Review the `## Unreleased` section against the commits since the previous tag (`git log <previous tag>..HEAD`), and fix missing or inaccurate entries.
3. Rename `## Unreleased` to `## [<version>] - <YYYY-MM-DD>` with today's date, and add a new empty `## Unreleased` section above it.
4. Set `version` in `Cargo.toml` and `.claude-plugin/plugin.json` to the new version, and run `cargo check` to update `Cargo.lock`.
5. Commit these files with the message `chore: release v<version>`.
6. Create an annotated tag: `git tag -a v<version> -m v<version>`.
7. Push the commit and then the tag: `git push origin main`, then `git push origin v<version>`.
8. Watch the Release workflow with `gh run watch`. It checks the versions and the changelog section, builds and tests every platform, and creates a draft GitHub Release whose notes are the changelog section, with the archives, `SHA256SUMS`, and build provenance attestations.
9. Check the draft with `gh release view v<version>`: the three archives and `SHA256SUMS` are attached, and the notes match the changelog. Report the result and the draft's URL to the maintainer.
10. Publish only when the maintainer tells you to: `gh release edit v<version> --draft=false`.

When the workflow fails, report the failure and fix its cause on `main`. If the fix changes released files, delete the tag locally and on `origin`, and start again from step 3 with the same version, because no release was published.
