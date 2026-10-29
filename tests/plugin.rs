//! Checks that the Claude Code plugin files agree with each other.
//!
//! `claude plugin validate .` is the full check. These tests catch the mismatches that it reports only at install
//! time, and they run without the Claude Code CLI.

use std::fs;
use std::path::Path;

use serde_json::Value;

fn read_json(path: &str) -> Value {
    let text = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn marketplace_entry_matches_plugin_manifest() {
    let plugin = read_json(".claude-plugin/plugin.json");
    let marketplace = read_json(".claude-plugin/marketplace.json");
    let entries = marketplace["plugins"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], plugin["name"]);
    assert_eq!(entries[0]["source"], "./");

    // Claude Code reserves plugin names that start with `claude-`.
    let name = plugin["name"].as_str().unwrap();
    assert!(!name.starts_with("claude-"), "{name}");
}

#[test]
fn plugin_version_matches_crate_version() {
    let plugin = read_json(".claude-plugin/plugin.json");
    assert_eq!(plugin["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn each_skill_names_its_folder() {
    let skills = Path::new(env!("CARGO_MANIFEST_DIR")).join("skills");
    for entry in fs::read_dir(skills).unwrap() {
        let dir = entry.unwrap().path();
        let text = fs::read_to_string(dir.join("SKILL.md"))
            .unwrap()
            .replace("\r\n", "\n");
        let frontmatter = text
            .strip_prefix("---\n")
            .unwrap()
            .split("\n---\n")
            .next()
            .unwrap();
        let folder = dir.file_name().unwrap().to_str().unwrap();
        assert!(
            frontmatter
                .lines()
                .any(|line| line == format!("name: {folder}")),
            "{folder}"
        );
        assert!(
            frontmatter
                .lines()
                .any(|line| line.starts_with("description: ")),
            "{folder}"
        );
    }
}
