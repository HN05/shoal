//! User-level skill delivery; no daemon, repository, or agent process is needed.
mod record;

use crate::fsutil::{self, Permissions, ReplaceOptions};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde_json::json;

use crate::{
    cli::{SkillCommand, SkillName},
    paths::Paths,
};
use record::{Entry, Record};

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
/// The single skill installed before Shoal recorded its installations.
const RETIRED: &str = "shoal";

pub(super) fn run(
    name: SkillName,
    command: Option<&SkillCommand>,
    json_output: bool,
    state_dir: Option<PathBuf>,
) -> Result<i32> {
    let Some(SkillCommand::Install { agent, force }) = command else {
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
        !crate::env::inherits_scope(&Paths::state_dir(state_dir)?),
        "workspace processes cannot install user-level skills; run shoal skill install outside the scoped execution"
    );
    let home = crate::fsutil::home_dir()?;
    ensure!(home.is_absolute(), "HOME must be an absolute path");
    let directories = crate::ai::skill_dirs(&crate::ai::load(&home)?, &home)?;
    let destinations: Vec<_> = if agent == "all" {
        // Only tools in use: a missing directory means the tool has no skills here.
        directories
            .into_iter()
            .filter_map(|(name, directory)| Some((name, directory?)))
            .filter(|(_, directory)| directory.is_dir())
            .collect()
    } else {
        let directory = directories.get(agent.as_str()).with_context(|| {
            format!("unknown AI tool {agent:?}; configure [ai.{agent}] with skill_dir in global Shoal config")
        })?;
        let directory = directory.clone().with_context(|| {
            format!("AI tool {agent:?} has no skill directory; set [ai.{agent}] skill_dir in global Shoal config")
        })?;
        vec![(agent.clone(), directory)]
    };
    let source = packaged()?;
    let mut installed = Vec::new();
    let mut kept = Vec::new();
    // Tools that share a directory share one installation.
    let mut outcomes = BTreeMap::new();
    for (agent, directory) in destinations {
        if !outcomes.contains_key(&directory) {
            let outcome = refresh(
                &directory,
                source.as_deref(),
                Mode::Install { force: *force },
            )?;
            outcomes.insert(directory.clone(), outcome);
        }
        let outcome = &outcomes[&directory];
        for name in &outcome.installed {
            let path = skill_file(&directory, name);
            if !json_output {
                println!("Installed {agent} skill {name} at {}", path.display());
            }
            installed.push(json!({"agent": agent, "skill": name, "path": path}));
        }
        for name in &outcome.kept {
            let path = skill_file(&directory, name);
            if !json_output {
                println!(
                    "Kept modified {agent} skill {name} at {}; use --force to replace it",
                    path.display()
                );
            }
            kept.push(json!({"agent": agent, "skill": name, "path": path}));
        }
    }
    if json_output {
        println!("{}", json!({"installed": installed, "kept": kept}));
    } else if outcomes.is_empty() {
        println!("No skill directories found; run `shoal skill install <tool>` to create one");
    }
    Ok(0)
}

/// Bring skills Shoal installed earlier up to date with this version, so an
/// upgrade needs no `shoal skill install`. Never fails the calling command.
pub(super) fn refresh_installed(state: &Path, json_output: bool) {
    if crate::env::inherits_scope(state) {
        return;
    }
    let warn = |error: anyhow::Error| {
        if !json_output {
            eprintln!("warning: cannot update Shoal skills: {error:#}");
        }
    };
    let found = installed_directories().and_then(|directories| Ok((directories, packaged()?)));
    let (directories, source) = match found {
        Ok(found) => found,
        Err(error) => return warn(error),
    };
    for directory in directories {
        match refresh(&directory, source.as_deref(), Mode::Refresh) {
            Ok(outcome) if !json_output => {
                if outcome.changed {
                    eprintln!("Updated Shoal skills in {}", directory.display());
                }
                for name in outcome.kept {
                    eprintln!(
                        "Kept modified skill {name} at {}; run `shoal skill install --force` to replace it",
                        skill_file(&directory, &name).display()
                    );
                }
            }
            Ok(_) => {}
            Err(error) => warn(error),
        }
    }
}

/// Every configured skill directory, once; `refresh` skips those without Shoal skills.
fn installed_directories() -> Result<BTreeSet<PathBuf>> {
    let home = crate::fsutil::home_dir()?;
    ensure!(home.is_absolute(), "HOME must be an absolute path");
    let directories = crate::ai::skill_dirs(&crate::ai::load(&home)?, &home)?;
    Ok(directories.into_values().flatten().collect())
}

fn packaged() -> Result<Option<PathBuf>> {
    let configured = std::env::var_os(crate::env::SKILLS_DIR)
        .map(PathBuf::from)
        .or_else(|| crate::env::COMPILED_SKILLS_DIR.map(PathBuf::from));
    packaged_source(configured, &std::env::current_exe()?)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// `shoal skill install`: install every bundled skill the user has not
    /// changed, or every one with `force`.
    Install { force: bool },
    /// After an upgrade: update only directories that hold Shoal skills, and
    /// leave skills the user removed or changed.
    Refresh,
}

#[derive(Debug, Default)]
struct Outcome {
    /// Bundled skills installed as this version provides them.
    installed: Vec<&'static str>,
    /// Skills left as the user changed them: on install every one, on refresh
    /// those found changed by this call.
    kept: Vec<String>,
    /// Whether this call changed a skill.
    changed: bool,
}

/// Install the bundled skills in `directory` and remove retired ones Shoal
/// still owns. A recorded skill whose file differs from what Shoal wrote
/// belongs to the user, as does every skill whose directory is not a real
/// directory, such as a link into the user's dotfiles; files from before the
/// record existed are Shoal's.
fn refresh(directory: &Path, source: Option<&Path>, mode: Mode) -> Result<Outcome> {
    let loaded = Record::load(directory)?;
    let mut record = loaded.clone().unwrap_or_default();
    let mut outcome = Outcome::default();
    if mode == Mode::Refresh && !holds_shoal_skills(directory, &record)? {
        return Ok(outcome);
    }
    let force = mode == (Mode::Install { force: true });
    for (name, contents) in SKILLS {
        let path = skill_file(directory, name);
        let expected = match source {
            Some(source) => Entry::Link(skill_file(source, name)),
            None => Entry::copy(contents.as_bytes()),
        };
        let present = exists(&path)?;
        let linked = !real_directory(&directory.join(name))?;
        // A concurrent refresh may already have written this version.
        let current = expected.matches(&path)?;
        let owned = match record.skills.get(name) {
            None => true,
            Some(Entry::Kept) => !present && mode != Mode::Refresh,
            Some(previous) if present => current || previous.matches(&path)?,
            Some(_) => mode != Mode::Refresh,
        };
        if (owned || force) && !linked {
            if !current {
                install(
                    &path,
                    source.map(|source| skill_file(source, name)).as_deref(),
                    contents,
                )?;
                outcome.changed = true;
            }
            record.skills.insert(name.to_owned(), expected);
            outcome.installed.push(name);
        } else if record.skills.insert(name.to_owned(), Entry::Kept) != Some(Entry::Kept) {
            if present {
                outcome.kept.push(name.to_owned());
            }
        } else if present && mode != Mode::Refresh {
            outcome.kept.push(name.to_owned());
        }
    }
    let retired: Vec<_> = record
        .skills
        .keys()
        .filter(|name| !SKILLS.iter().any(|(bundled, _)| bundled == name))
        .cloned()
        .collect();
    for name in retired {
        let entry = record
            .skills
            .remove(&name)
            .expect("retired names are recorded");
        let path = skill_file(directory, &name);
        if real_directory(&directory.join(&name))? && entry.matches(&path)? {
            remove_skill(&directory.join(&name))?;
            outcome.changed = true;
        } else if entry != Entry::Kept && exists(&path)? {
            outcome.kept.push(name);
        }
    }
    if loaded.is_none()
        && real_directory(&directory.join(RETIRED))?
        && remove_skill(&directory.join(RETIRED))?
    {
        outcome.changed = true;
    }
    if loaded.as_ref() != Some(&record) {
        fs::create_dir_all(directory)
            .with_context(|| format!("create skill directory {}", directory.display()))?;
        record.save(directory)?;
    }
    Ok(outcome)
}

/// Whether `directory` still holds a skill Shoal installed, now or earlier;
/// removing every one stops automatic updates there.
fn holds_shoal_skills(directory: &Path, record: &Record) -> Result<bool> {
    let names = SKILLS
        .iter()
        .map(|(name, _)| *name)
        .chain(record.skills.keys().map(String::as_str))
        .chain([RETIRED]);
    for name in names {
        if exists(&skill_file(directory, name))? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether `path` is a directory itself, not a link to one, or is absent.
fn real_directory(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error) if record::is_absent(&error) => Ok(true),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if record::is_absent(&error) => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
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

/// Remove a retired skill's `SKILL.md`, which Shoal owned, and its directory
/// once empty; other files keep the directory. Returns whether it was there.
fn remove_skill(directory: &Path) -> Result<bool> {
    let path = directory.join("SKILL.md");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.is_dir() => match fs::remove_file(&path) {
            // A concurrent refresh removed it first.
            Err(error) if record::is_absent(&error) => return Ok(false),
            result => result.with_context(|| format!("remove retired skill {}", path.display()))?,
        },
        Ok(_) => return Ok(false),
        Err(error) if record::is_absent(&error) => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    }
    match fs::remove_dir(directory) {
        Err(error)
            if error.kind() == io::ErrorKind::DirectoryNotEmpty || record::is_absent(&error) =>
        {
            Ok(true)
        }
        result => result
            .map(|()| true)
            .with_context(|| format!("remove {}", directory.display())),
    }
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

    fn record(directory: &Path, entries: &[(&str, Entry)]) {
        let mut record = Record::load(directory).unwrap().unwrap_or_default();
        for (name, entry) in entries {
            record.skills.insert((*name).to_owned(), entry.clone());
        }
        record.save(directory).unwrap();
    }

    #[test]
    fn install_replaces_skills_shoal_owns_and_keeps_user_changes() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path();
        let (worker, orchestrator) = (SKILLS[0].0, SKILLS[1].0);
        let read = |name| fs::read_to_string(skill_file(directory, name)).unwrap();
        // Skills from before the record existed are Shoal's.
        for (name, _) in SKILLS {
            fs::create_dir_all(directory.join(name)).unwrap();
            fs::write(skill_file(directory, name), "old").unwrap();
        }
        refresh(directory, None, Mode::Install { force: false }).unwrap();
        assert_eq!(read(worker), SKILLS[0].1);
        // A recorded copy from an earlier version is replaced; an edited one is kept.
        fs::write(skill_file(directory, worker), "old").unwrap();
        fs::write(skill_file(directory, orchestrator), "edited").unwrap();
        record(
            directory,
            &[
                (worker, Entry::copy(b"old")),
                (orchestrator, Entry::copy(b"old")),
            ],
        );
        for _ in 0..2 {
            let outcome = refresh(directory, None, Mode::Install { force: false }).unwrap();
            assert_eq!(outcome.installed, [worker]);
            assert_eq!(outcome.kept, [orchestrator]);
            assert_eq!(read(worker), SKILLS[0].1);
            assert_eq!(read(orchestrator), "edited");
        }
        let outcome = refresh(directory, None, Mode::Install { force: true }).unwrap();
        assert!(outcome.kept.is_empty());
        assert_eq!(read(orchestrator), SKILLS[1].1);
        // A removed skill is restored.
        fs::remove_file(skill_file(directory, worker)).unwrap();
        refresh(directory, None, Mode::Install { force: false }).unwrap();
        assert_eq!(read(worker), SKILLS[0].1);
        // A linked skill directory is the user's, even when forced.
        let elsewhere = tempfile::tempdir().unwrap();
        fs::write(elsewhere.path().join("SKILL.md"), "old").unwrap();
        fs::remove_dir_all(directory.join(worker)).unwrap();
        symlink(elsewhere.path(), directory.join(worker)).unwrap();
        record(directory, &[(worker, Entry::copy(b"old"))]);
        let outcome = refresh(directory, None, Mode::Install { force: true }).unwrap();
        assert_eq!(outcome.kept, [worker]);
        assert_eq!(read(worker), "old");
    }

    #[test]
    fn install_removes_retired_skills_only_while_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("skills");
        let source = root.path().join("package");
        for (name, _) in SKILLS {
            fs::create_dir_all(source.join(name)).unwrap();
            fs::write(skill_file(&source, name), "packaged").unwrap();
        }
        refresh(&directory, Some(&source), Mode::Install { force: false }).unwrap();
        for name in ["linked", "copied", "edited", "kept"] {
            fs::create_dir_all(directory.join(name)).unwrap();
        }
        // An upgraded package no longer holds the linked skill.
        symlink(
            skill_file(&source, "linked"),
            skill_file(&directory, "linked"),
        )
        .unwrap();
        fs::write(skill_file(&directory, "copied"), "retired").unwrap();
        fs::write(directory.join("copied/notes.md"), "keep").unwrap();
        fs::write(skill_file(&directory, "edited"), "edited").unwrap();
        fs::write(skill_file(&directory, "kept"), "kept").unwrap();
        // A retired skill reached through a linked directory stays.
        let elsewhere = root.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        fs::write(elsewhere.join("SKILL.md"), "retired").unwrap();
        symlink(&elsewhere, directory.join("redirected")).unwrap();
        record(
            &directory,
            &[
                ("redirected", Entry::copy(b"retired")),
                ("linked", Entry::Link(skill_file(&source, "linked"))),
                ("copied", Entry::copy(b"retired")),
                ("edited", Entry::copy(b"retired")),
                ("kept", Entry::Kept),
            ],
        );
        let outcome = refresh(&directory, Some(&source), Mode::Install { force: false }).unwrap();
        assert_eq!(outcome.kept, ["edited", "redirected"]);
        assert!(elsewhere.join("SKILL.md").exists());
        assert!(!directory.join("linked").exists());
        assert_eq!(fs::read_dir(directory.join("copied")).unwrap().count(), 1);
        for name in ["edited", "kept"] {
            assert!(skill_file(&directory, name).exists());
        }
        let names: Vec<_> = Record::load(&directory)
            .unwrap()
            .unwrap()
            .skills
            .into_keys()
            .collect();
        assert_eq!(names, [SKILLS[1].0, SKILLS[0].0]);
        // A retired skill is reported once, then belongs to the user.
        let outcome = refresh(&directory, Some(&source), Mode::Install { force: false }).unwrap();
        assert!(outcome.kept.is_empty());
    }

    #[test]
    fn refresh_updates_installed_skills_and_reports_user_changes_once() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path();
        let (worker, orchestrator) = (SKILLS[0].0, SKILLS[1].0);
        let read = |name| fs::read_to_string(skill_file(directory, name)).unwrap();
        // Refreshing never starts an installation.
        let outcome = refresh(directory, None, Mode::Refresh).unwrap();
        assert!(!outcome.changed && outcome.installed.is_empty());
        assert_eq!(fs::read_dir(directory).unwrap().count(), 0);

        // An upgrade replaces what an earlier version wrote.
        for (name, _) in SKILLS {
            fs::create_dir_all(directory.join(name)).unwrap();
            fs::write(skill_file(directory, name), "old").unwrap();
        }
        record(directory, &[(worker, Entry::copy(b"old"))]);
        fs::write(skill_file(directory, orchestrator), "edited").unwrap();
        record(directory, &[(orchestrator, Entry::copy(b"old"))]);
        let outcome = refresh(directory, None, Mode::Refresh).unwrap();
        assert!(outcome.changed);
        assert_eq!(outcome.installed, [worker]);
        assert_eq!(outcome.kept, [orchestrator]);
        assert_eq!(read(worker), SKILLS[0].1);
        assert_eq!(read(orchestrator), "edited");
        // A kept skill is reported once by refresh and on every install.
        let outcome = refresh(directory, None, Mode::Refresh).unwrap();
        assert!(!outcome.changed && outcome.kept.is_empty());
        let outcome = refresh(directory, None, Mode::Install { force: false }).unwrap();
        assert_eq!(outcome.kept, [orchestrator]);
        assert_eq!(read(orchestrator), "edited");
        refresh(directory, None, Mode::Install { force: true }).unwrap();
        assert_eq!(read(orchestrator), SKILLS[1].1);

        // A skill the user removed returns only on install.
        fs::remove_file(skill_file(directory, worker)).unwrap();
        record(directory, &[(worker, Entry::copy(b"old"))]);
        let outcome = refresh(directory, None, Mode::Refresh).unwrap();
        assert!(outcome.kept.is_empty() && !skill_file(directory, worker).exists());
        refresh(directory, None, Mode::Refresh).unwrap();
        assert!(!skill_file(directory, worker).exists());
        refresh(directory, None, Mode::Install { force: false }).unwrap();
        assert_eq!(read(worker), SKILLS[0].1);

        // Removing every Shoal skill stops refreshes there.
        for (name, _) in SKILLS {
            fs::remove_dir_all(directory.join(name)).unwrap();
        }
        record(directory, &[("shoal-new", Entry::Kept)]);
        refresh(directory, None, Mode::Refresh).unwrap();
        assert!(!directory.join(worker).exists());
    }

    #[test]
    fn retired_skill_is_removed_without_other_files() {
        let root = tempfile::tempdir().unwrap();
        let retired = root.path().join(RETIRED);
        remove_skill(&retired).unwrap();
        fs::create_dir(&retired).unwrap();
        // A dangling link from an upgraded package is removed like a copy.
        symlink(root.path().join("missing.md"), retired.join("SKILL.md")).unwrap();
        fs::write(retired.join("notes.md"), "keep").unwrap();
        remove_skill(&retired).unwrap();
        assert_eq!(fs::read_dir(&retired).unwrap().count(), 1);
        fs::write(retired.join("SKILL.md"), "old").unwrap();
        fs::remove_file(retired.join("notes.md")).unwrap();
        remove_skill(&retired).unwrap();
        assert!(!retired.exists());
        fs::create_dir_all(retired.join("SKILL.md")).unwrap();
        remove_skill(&retired).unwrap();
        assert!(retired.join("SKILL.md").is_dir());
    }
}
