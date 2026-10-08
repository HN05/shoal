//! User-level skill delivery; no daemon, repository, or agent process is needed.
use crate::fsutil::{self, Permissions, ReplaceOptions};
use std::{
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde_json::json;

use crate::cli::{SkillCommand, SkillName};

/// Bundled skills by name; a packaged directory holds `<name>/SKILL.md` for each.
const SKILLS: [(&str, &str); 2] = [
    (
        "shoal-worker",
        include_str!("../../../skills/shoal-worker/SKILL.md"),
    ),
    (
        "shoal-orchestrator",
        include_str!("../../../skills/shoal-orchestrator/SKILL.md"),
    ),
];

pub(super) fn run(
    name: SkillName,
    command: Option<&SkillCommand>,
    json_output: bool,
) -> Result<i32> {
    let Some(SkillCommand::Install { agent }) = command else {
        let (name, contents) = match name {
            SkillName::Worker => SKILLS[0],
            SkillName::Orchestrator => SKILLS[1],
        };
        if json_output {
            println!("{}", json!({"name": name, "skill": contents}));
        } else {
            print!("{contents}");
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
            Ok((name, directory))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut installed = Vec::new();
    let configured_source = std::env::var_os(crate::env::SKILLS_DIR)
        .map(PathBuf::from)
        .or_else(|| crate::env::COMPILED_SKILLS_DIR.map(PathBuf::from));
    let source = packaged_source(configured_source, &std::env::current_exe()?)?;
    for (agent, directory) in destinations {
        for (name, contents) in SKILLS {
            let path = skill_file(&directory, name);
            let packaged = source.as_ref().map(|source| skill_file(source, name));
            install(&path, packaged.as_deref(), contents)?;
            if !json_output {
                println!("Installed {agent} skill {name} at {}", path.display());
            }
            installed.push(json!({"agent": agent, "skill": name, "path": path}));
        }
    }
    if json_output {
        println!("{}", json!({"installed": installed}));
    }
    Ok(0)
}

fn packaged_source(configured: Option<PathBuf>, executable: &Path) -> Result<Option<PathBuf>> {
    let source = match configured {
        Some(source) => Some(source),
        None => packaged_link_source(executable)?,
    };
    if let Some(source) = &source {
        ensure!(
            source.is_absolute(),
            "packaged skills directory must be absolute"
        );
        for (name, _) in SKILLS {
            let file = skill_file(source, name);
            ensure!(
                file.is_file(),
                "packaged skill is missing: {}",
                file.display()
            );
        }
    }
    Ok(source)
}

fn skill_file(directory: &Path, name: &str) -> PathBuf {
    directory.join(name).join("SKILL.md")
}

fn packaged_link_source(executable: &Path) -> Result<Option<PathBuf>> {
    let link = fs::canonicalize(executable)
        .context("resolve packaged executable")?
        .with_file_name("shoal-skills");
    let target = match fs::read_link(&link) {
        Ok(target) => target,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("read packaged shoal-skills link"),
    };
    if target.is_absolute() {
        return Ok(Some(target));
    }
    let absolute = link
        .parent()
        .context("missing packaged skills link directory")?
        .join(target);
    // Homebrew relativizes this link. Resolve it lexically so the installed
    // skills follow the stable opt prefix rather than a versioned Cellar path.
    let mut source = PathBuf::new();
    for component in absolute.components() {
        if component == std::path::Component::ParentDir {
            source.pop();
        } else {
            source.push(component.as_os_str());
        }
    }
    Ok(Some(source))
}

fn install(path: &Path, source: Option<&Path>, contents: &str) -> Result<()> {
    let directory = path.parent().context("missing skill directory")?;
    fs::create_dir_all(directory)
        .with_context(|| format!("create skill directory {}", directory.display()))?;
    // Replace only SKILL.md atomically, never following its previous symlink.
    if let Some(source) = source {
        let temporary = tempfile::tempdir_in(directory)?;
        let link = temporary.path().join("SKILL.md");
        symlink(source, &link)?;
        fs::rename(link, path).with_context(|| format!("link skill at {}", path.display()))?;
    } else {
        fsutil::replace_atomically(
            path,
            contents.as_bytes(),
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
        install(&path, None, "embedded").unwrap();
        assert!(!path.is_symlink());
        assert_eq!(fs::read_to_string(&path).unwrap(), "embedded");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(fs::read_to_string(source).unwrap(), "personal");
    }

    #[test]
    fn packaged_skills_link_is_optional_and_explicit_paths_take_precedence() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("shoal");
        let link = root.path().join("shoal-skills");
        let source = root.path().join("skills");
        let incomplete = root.path().join("incomplete");
        fs::write(&executable, "binary").unwrap();
        for (name, _) in SKILLS {
            fs::create_dir_all(source.join(name)).unwrap();
            fs::write(skill_file(&source, name), "packaged").unwrap();
        }
        fs::create_dir_all(incomplete.join(SKILLS[0].0)).unwrap();
        fs::write(skill_file(&incomplete, SKILLS[0].0), "packaged").unwrap();
        assert_eq!(packaged_source(None, &executable).unwrap(), None);
        symlink(&source, &link).unwrap();
        assert_eq!(
            packaged_source(None, &executable).unwrap(),
            Some(source.clone())
        );
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let launcher = bin.join("shoal");
        symlink(&executable, &launcher).unwrap();
        assert_eq!(
            packaged_source(None, &launcher).unwrap(),
            Some(source.clone())
        );
        // Preserve the stable symlink target instead of canonicalizing it to a
        // versioned package path that disappears on upgrade.
        let stable = root.path().join("stable");
        symlink(&source, &stable).unwrap();
        assert_eq!(
            packaged_source(Some(stable.clone()), &executable).unwrap(),
            Some(stable)
        );
        for invalid in [
            PathBuf::from("relative"),
            root.path().join("missing"),
            incomplete,
            executable.clone(),
        ] {
            assert!(packaged_source(Some(invalid.clone()), &executable).is_err());
            fs::remove_file(&link).unwrap();
            symlink(&invalid, &link).unwrap();
            assert!(packaged_source(None, &executable).is_err());
            assert_eq!(
                packaged_source(Some(source.clone()), &executable).unwrap(),
                Some(source.clone())
            );
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
            install(&destination, Some(&source), "embedded").unwrap();
            assert_eq!(fs::read_link(&destination).unwrap(), source);
            assert_eq!(fs::read_to_string(&old).unwrap(), "personal");
        }
        fs::write(&source, "version two").unwrap();
        assert_eq!(fs::read_to_string(&destination).unwrap(), "version two");
        fs::remove_file(&destination).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("keep"), "keep").unwrap();
        assert!(install(&destination, Some(&source), "embedded").is_err());
        assert_eq!(
            fs::read_to_string(destination.join("keep")).unwrap(),
            "keep"
        );
    }
}
