//! User-level skill delivery; no daemon, repository, or agent process is needed.
use crate::fsutil::{self, Permissions, ReplaceOptions};
use std::{
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde_json::json;

use crate::cli::SkillCommand;

const SKILL: &str = include_str!("../../../SKILL.md");

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
        !crate::env::is_scoped(),
        "workspace processes cannot install user-level skills; run shoal skill install outside the scoped execution"
    );
    let home = crate::fsutil::home_dir()?;
    ensure!(home.is_absolute(), "HOME must be an absolute path");
    let configured = crate::ai::load(&home)?;
    let mut names = std::collections::BTreeSet::from(crate::ai::BUILT_INS);
    names.extend(configured.keys().map(String::as_str));
    ensure!(
        agent == "all" || names.contains(agent.as_str()),
        "unknown AI tool {agent:?}; configure [ai.{agent}] with skill_dir in global Shoal config"
    );
    let destinations = names
        .into_iter()
        .filter(|name| agent == "all" || agent == name)
        .map(|name| {
            let directory = match configured.get(name) {
                Some(settings) => crate::ai::skill_dir(settings, &home)?,
                None if name == "claude" => crate::env::claude_config_dir()?
                    .unwrap_or_else(|| home.join(".claude"))
                    .join("skills"),
                None => home.join(".agents/skills"),
            };
            Ok((name, directory.join("shoal/SKILL.md")))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut installed = Vec::new();
    let source = packaged_source(
        std::env::var_os(crate::env::SKILL_PATH).map(PathBuf::from),
        crate::env::COMPILED_SKILL_PATH.map(PathBuf::from),
    )?;
    for (agent, path) in destinations {
        let directory = path.parent().context("missing skill directory")?;
        fs::create_dir_all(directory)
            .with_context(|| format!("create skill directory {}", directory.display()))?;
        install(&path, source.as_deref())?;
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

fn packaged_source(runtime: Option<PathBuf>, compiled: Option<PathBuf>) -> Result<Option<PathBuf>> {
    let source = runtime.or(compiled);
    if let Some(source) = &source {
        ensure!(source.is_absolute(), "packaged skill path must be absolute");
        ensure!(
            source.is_file(),
            "packaged skill is missing: {}",
            source.display()
        );
    }
    Ok(source)
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
        fsutil::replace_atomically(
            path,
            SKILL.as_bytes(),
            ReplaceOptions {
                permissions: Permissions::Mode(0o644),
                sync: false,
            },
        )
        .with_context(|| format!("install skill at {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_skill_replaces_symlinks_with_a_readable_file() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("personal.md");
        let path = root.path().join("SKILL.md");
        fs::write(&source, "personal").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&source, &path).unwrap();
        install(&path, None).unwrap();
        assert!(!path.is_symlink());
        assert_eq!(fs::read_to_string(&path).unwrap(), SKILL);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(fs::read_to_string(source).unwrap(), "personal");
    }

    #[test]
    fn runtime_skill_path_overrides_build_path_and_requires_a_file() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("SKILL.md");
        let missing = root.path().join("missing.md");
        fs::write(&source, "packaged").unwrap();
        assert_eq!(
            packaged_source(Some(source.clone()), Some(missing.clone())).unwrap(),
            Some(source.clone())
        );
        assert_eq!(
            packaged_source(None, Some(source.clone())).unwrap(),
            Some(source.clone())
        );
        assert_eq!(packaged_source(None, None).unwrap(), None);
        for invalid in [
            PathBuf::from("relative.md"),
            missing,
            root.path().to_owned(),
        ] {
            assert!(packaged_source(Some(invalid), Some(source.clone())).is_err());
        }
    }

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
