//! User-level skill delivery; no daemon, repository, or agent process is needed.
use std::{fs, io::Write, os::unix::fs::PermissionsExt, path::PathBuf};

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
    for (agent, path) in destinations {
        let directory = path.parent().context("missing skill directory")?;
        fs::create_dir_all(directory)
            .with_context(|| format!("create skill directory {}", directory.display()))?;
        // Replace only this file, atomically. Do not truncate an existing file
        // or follow a SKILL.md symlink into a user's source checkout.
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        temporary.write_all(SKILL.as_bytes())?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o644))?;
        temporary
            .persist(&path)
            .with_context(|| format!("install skill at {}", path.display()))?;
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
