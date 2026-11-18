# Agent instructions

- Before committing, run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`. CI runs the same checks on Windows x64, Windows Arm64, and macOS on Apple silicon.
- Add a changelog entry for every change that users notice, and cut releases as described in [docs/releasing.md](docs/releasing.md).
