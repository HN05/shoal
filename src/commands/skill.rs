//! User-level skill delivery; no daemon, repository, or agent process is needed.
use std::{
    fs,
    io::Write,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde_json::json;

use crate::cli::{SkillAgent, SkillCommand};

const SKILL: &str = include_str!("../../SKILL.md");

pub(super) fn run(command: Option<&SkillCommand>, json_output: bool) -> Result<i32> {
    let Some(SkillCommand::Install { agent }) = command else {
        if json_output {
            println!("{}", json!({"skill": SKILL}));
        } else {
            print!("{SKILL}");
        }
        return Ok(0);
    };
    ensure!(
        std::env::var_os("SHOAL_SCOPE_TOKEN").is_none(),
        "workspace processes cannot install user-level skills; run shoal skill install outside the scoped execution"
    );
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    ensure!(home.is_absolute(), "HOME must be an absolute path");
    let mut destinations = Vec::new();
    if matches!(agent, SkillAgent::All | SkillAgent::Codex) {
        destinations.push(("codex", home.join(".agents/skills/shoal/SKILL.md")));
    }
    if matches!(agent, SkillAgent::All | SkillAgent::Claude) {
        let config = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"));
        ensure!(
            config.is_absolute(),
            "CLAUDE_CONFIG_DIR must be an absolute path"
        );
        destinations.push(("claude", config.join("skills/shoal/SKILL.md")));
    }
    let mut installed = Vec::new();
    let source = option_env!("SHOAL_SKILL_PATH").map(Path::new);
    if let Some(source) = source {
        ensure!(source.is_absolute(), "packaged skill path must be absolute");
        ensure!(
            source.is_file(),
            "packaged skill is missing: {}",
            source.display()
        );
    }
    for (agent, path) in destinations {
        let directory = path.parent().context("missing skill directory")?;
        fs::create_dir_all(directory)
            .with_context(|| format!("create skill directory {}", directory.display()))?;
        install(&path, source)?;
        if !json_output {
            println!("Installed {agent} skill at {}", path.display());
        }
        installed.push(json!({"agent": agent, "path": path}));
    }
    if json_output {
        println!("{}", json!({"installed": installed}));
    }
    Ok(0)
}

fn install(path: &Path, source: Option<&Path>) -> Result<()> {
    let directory = path.parent().context("missing skill directory")?;
    // Replace only SKILL.md atomically, never following its previous symlink.
    if let Some(source) = source {
        let temporary = tempfile::tempdir_in(directory)?;
        let link = temporary.path().join("SKILL.md");
        symlink(source, &link)?;
        fs::rename(link, path).with_context(|| format!("link skill at {}", path.display()))?;
    } else {
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        temporary.write_all(SKILL.as_bytes())?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o644))?;
        temporary
            .persist(path)
            .with_context(|| format!("install skill at {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaged_skill_tracks_upgrades_and_preserves_previous_source() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("personal.md");
        let source = root.path().join("packaged.md");
        let destination = root.path().join("SKILL.md");
        fs::write(&old, "personal").unwrap();
        fs::write(&source, "version one").unwrap();
        symlink(&old, &destination).unwrap();
        for _ in 0..2 {
            install(&destination, Some(&source)).unwrap();
            assert_eq!(fs::read_link(&destination).unwrap(), source);
            assert_eq!(fs::read_to_string(&old).unwrap(), "personal");
        }
        fs::write(&source, "version two").unwrap();
        assert_eq!(fs::read_to_string(&destination).unwrap(), "version two");
        fs::remove_file(&destination).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("keep"), "keep").unwrap();
        assert!(install(&destination, Some(&source)).is_err());
        assert_eq!(
            fs::read_to_string(destination.join("keep")).unwrap(),
            "keep"
        );
    }
}
