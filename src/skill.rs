//! Installs the Claude Code skill that teaches Claude how to use this CLI.
//!
//! The skill is compiled into the executable, so the installed skill always matches the CLI that installed it.

use std::path::{Path, PathBuf};

use anyhow::Context;

/// The skill's `SKILL.md`.
pub const SKILL_MD: &str = include_str!("../skills/claude-account-sync/SKILL.md");

/// The skill's folder name, which Claude Code uses as the skill name.
const SKILL_NAME: &str = "claude-account-sync";

/// Returns the default skills folder: `~/.claude/skills`, where Claude Code finds personal skills.
pub fn default_skills_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".claude").join("skills"))
}

/// Writes `SKILL.md` to `<skills_dir>/claude-account-sync/` and returns its path. Replaces an older copy.
pub fn install(skills_dir: &Path) -> anyhow::Result<PathBuf> {
    let path = skills_dir.join(SKILL_NAME).join("SKILL.md");
    crate::file_ops::write_atomic(&path, SKILL_MD.as_bytes(), None)
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(path)
}

/// Removes `<skills_dir>/claude-account-sync/` when it exists.
pub fn uninstall(skills_dir: &Path) -> anyhow::Result<()> {
    let dir = skills_dir.join(SKILL_NAME);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("cannot remove {}", dir.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_has_frontmatter_with_matching_name() {
        let frontmatter = SKILL_MD
            .strip_prefix("---\n")
            .unwrap()
            .split("\n---\n")
            .next()
            .unwrap();
        assert!(
            frontmatter
                .lines()
                .any(|line| line == format!("name: {SKILL_NAME}"))
        );
        assert!(
            frontmatter
                .lines()
                .any(|line| line.starts_with("description: "))
        );
    }

    #[test]
    fn install_and_uninstall_skill() {
        let dir = tempfile::tempdir().unwrap();
        let path = install(dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SKILL_MD);
        uninstall(dir.path()).unwrap();
        assert!(!path.exists());
    }
}
