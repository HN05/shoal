//! User-level skill delivery; no daemon, repository, or agent process is needed.
mod record;

use crate::fsutil::{self, Permissions, ReplaceOptions};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, ensure};
use serde_json::json;

use crate::{
    cli::{
        SkillCommand, SkillName, context,
        ui::{self, ModifiedSkillChoice},
    },
    paths::Paths,
    tools::Tool,
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

/// Each bundled skill's name and the contents this version installs.
type Contents = Vec<(&'static str, String)>;

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
            outln!("{}", json!({"name": name, "skill": contents}));
        } else {
            out!("{contents}");
        }
        return Ok(0);
    };
    ensure!(
        !crate::env::inherits_scope(&Paths::state_dir(state_dir)?),
        "workspace processes cannot install user-level skills; run shoal skill install outside the scoped execution"
    );
    ensure!(
        crate::build::changes_user_files(),
        "development builds cannot install user-level skills; set {}=1 in a temporary HOME to test installation",
        crate::env::DEV_USER_FILES
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
    let skills = packaged()?;
    let changed = if *force {
        Changed::Replace
    } else if context::Context::is_interactive(json_output) {
        Changed::Ask
    } else {
        Changed::Keep
    };
    // Tools that share a directory share one installation, reported once.
    let mut agents: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for (agent, directory) in destinations {
        agents.entry(directory).or_default().push(agent);
    }
    let mut installed = Vec::new();
    let mut kept = Vec::new();
    for (directory, agents) in &agents {
        let outcome = refresh(directory, &skills, Mode::Install(changed))?;
        for name in &outcome.installed {
            let path = skill_file(directory, name);
            if !json_output {
                outln!("Installed skill {name} at {}", path.display());
            }
            installed.push(json!({"agents": agents, "skill": name, "path": path}));
        }
        for name in &outcome.kept {
            let path = skill_file(directory, name);
            if !json_output {
                outln!(
                    "Kept modified skill {name} at {}; use --force to replace it",
                    path.display()
                );
            }
            kept.push(json!({"agents": agents, "skill": name, "path": path}));
        }
    }
    if json_output {
        outln!("{}", json!({"installed": installed, "kept": kept}));
    } else if agents.is_empty() {
        outln!("No skill directories found; run `shoal skill install <tool>` to create one");
    }
    Ok(0)
}

/// Bring skills Shoal installed earlier up to date with this version, so an
/// upgrade needs no `shoal skill install`. Never fails the calling command.
pub(super) fn refresh_installed(state: &Path, json_output: bool) {
    if crate::env::inherits_scope(state) || !crate::build::changes_user_files() {
        return;
    }
    let warn = |error: anyhow::Error| {
        if !json_output {
            eprintln!("warning: cannot update Shoal skills: {error:#}");
        }
    };
    let found = installed_directories().and_then(|directories| Ok((directories, packaged()?)));
    let (directories, skills) = match found {
        Ok(found) => found,
        Err(error) => return warn(error),
    };
    for directory in directories {
        match refresh(&directory, &skills, Mode::Refresh) {
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

fn packaged() -> Result<Contents> {
    let configured = std::env::var_os(crate::env::SKILLS_DIR)
        .map(PathBuf::from)
        .or_else(|| crate::env::COMPILED_SKILLS_DIR.map(PathBuf::from));
    let source = packaged_source(configured, &std::env::current_exe()?)?;
    skill_contents(source.as_deref())
}

/// The packaged skills when `source` names them, otherwise the embedded ones.
fn skill_contents(source: Option<&Path>) -> Result<Contents> {
    SKILLS
        .iter()
        .map(|(name, embedded)| {
            let contents = match source {
                Some(source) => {
                    let file = skill_file(source, name);
                    fs::read_to_string(&file)
                        .with_context(|| format!("read packaged skill {}", file.display()))?
                }
                None => (*embedded).to_owned(),
            };
            Ok((*name, contents))
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// `shoal skill install`: install every bundled skill the user has not
    /// changed, and decide about changed ones.
    Install(Changed),
    /// After an upgrade: update only directories that hold Shoal skills, and
    /// leave skills the user removed or changed.
    Refresh,
}

/// What an install does with a skill the user changed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Changed {
    Keep,
    Replace,
    Ask,
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
/// record existed are Shoal's. Skills are regular files, since some tools
/// skip a linked `SKILL.md`; links earlier versions recorded become copies.
fn refresh(directory: &Path, skills: &[(&'static str, String)], mode: Mode) -> Result<Outcome> {
    let loaded = Record::load(directory)?;
    let mut record = loaded.clone().unwrap_or_default();
    let mut outcome = Outcome::default();
    if mode == Mode::Refresh && !holds_shoal_skills(directory, &record)? {
        return Ok(outcome);
    }
    for &(name, ref contents) in skills {
        let path = skill_file(directory, name);
        let expected = Entry::copy(contents.as_bytes());
        let present = exists(&path)?;
        let linked = !real_directory(&directory.join(name))?;
        // A concurrent refresh may already have written this version.
        let current = expected.matches(&path)?;
        let owned = match record.skills.get(name) {
            None => true,
            // An install reclaims a kept skill that matches this version.
            Some(Entry::Kept) => (!present || current) && mode != Mode::Refresh,
            Some(previous) if present => current || previous.matches(&path)?,
            Some(_) => mode != Mode::Refresh,
        };
        if !linked && (owned || present && replace_changed(mode, name, &path, contents)?) {
            if !current {
                install(&path, contents)?;
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

/// Whether an install replaces the changed skill at `path`.
fn replace_changed(mode: Mode, name: &str, path: &Path, contents: &str) -> Result<bool> {
    match mode {
        Mode::Install(Changed::Replace) => Ok(true),
        Mode::Install(Changed::Ask) => loop {
            match ui::modified_skill_choice(name, path)? {
                ModifiedSkillChoice::Keep => return Ok(false),
                ModifiedSkillChoice::Replace => return Ok(true),
                ModifiedSkillChoice::Diff => show_diff(path, contents)?,
            }
        },
        Mode::Install(Changed::Keep) | Mode::Refresh => Ok(false),
    }
}

/// Print how `contents` would change the installed skill at `path`.
fn show_diff(path: &Path, contents: &str) -> Result<()> {
    let mut diff = Command::new(Tool::Diff.program())
        .args(["-u", "-L"])
        .arg(path)
        .args(["-L", "new version", "--"])
        .arg(path)
        .arg("-")
        .stdin(Stdio::piped())
        .spawn()
        .context("show skill changes; install diff")?;
    let mut input = diff.stdin.take().context("diff stdin is unavailable")?;
    io::Write::write_all(&mut input, contents.as_bytes()).context("send skill to diff")?;
    drop(input);
    let status = diff.wait()?;
    // diff exits with 1 when the files differ.
    ensure!(
        matches!(status.code(), Some(0 | 1)),
        "diff failed with {status}"
    );
    Ok(())
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
    // skills are read through the stable opt prefix rather than a Cellar path.
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

fn install(path: &Path, contents: &str) -> Result<()> {
    let directory = path.parent().context("missing skill directory")?;
    fs::create_dir_all(directory)
        .with_context(|| format!("create skill directory {}", directory.display()))?;
    // Replace only SKILL.md atomically, never following its previous symlink.
    fsutil::replace_atomically(
        path,
        contents.as_bytes(),
        ReplaceOptions {
            permissions: Permissions::Mode(0o644),
            sync: false,
        },
    )
    .with_context(|| format!("install skill at {}", path.display()))
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
    use std::os::unix::fs::symlink;

    fn embedded() -> Contents {
        skill_contents(None).unwrap()
    }

    #[test]
    fn embedded_skill_replaces_symlinks_with_a_readable_file() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("personal.md");
        let path = root.path().join("SKILL.md");
        fs::write(&source, "personal").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&source, &path).unwrap();
        install(&path, "embedded").unwrap();
        assert!(!path.is_symlink());
        assert_eq!(fs::read_to_string(&path).unwrap(), "embedded");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(fs::read_to_string(source).unwrap(), "personal");
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), "keep").unwrap();
        assert!(install(&path, "embedded").is_err());
        assert_eq!(fs::read_to_string(path.join("keep")).unwrap(), "keep");
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
    fn packaged_skills_replace_recorded_links_with_copies_that_follow_upgrades() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("skills");
        let source = root.path().join("package");
        let (worker, orchestrator) = (SKILLS[0].0, SKILLS[1].0);
        for (name, _) in SKILLS {
            fs::create_dir_all(source.join(name)).unwrap();
            fs::write(skill_file(&source, name), "version one").unwrap();
            fs::create_dir_all(directory.join(name)).unwrap();
        }
        fs::write(directory.join(worker).join("notes.md"), "keep").unwrap();
        // An earlier version linked both skills; the user since repointed one.
        let personal = root.path().join("personal.md");
        fs::write(&personal, "personal").unwrap();
        symlink(skill_file(&source, worker), skill_file(&directory, worker)).unwrap();
        symlink(&personal, skill_file(&directory, orchestrator)).unwrap();
        record(
            &directory,
            &[
                (worker, Entry::Link(skill_file(&source, worker))),
                (orchestrator, Entry::Link(skill_file(&source, orchestrator))),
            ],
        );
        let packaged = skill_contents(Some(&source)).unwrap();
        let outcome = refresh(&directory, &packaged, Mode::Refresh).unwrap();
        assert_eq!(outcome.installed, [worker]);
        assert_eq!(outcome.kept, [orchestrator]);
        let installed = skill_file(&directory, worker);
        assert!(!installed.is_symlink());
        assert_eq!(fs::read_to_string(&installed).unwrap(), "version one");
        assert_eq!(
            fs::read_link(skill_file(&directory, orchestrator)).unwrap(),
            personal
        );
        assert_eq!(
            fs::read_to_string(directory.join(worker).join("notes.md")).unwrap(),
            "keep"
        );
        assert_eq!(
            fs::read_to_string(skill_file(&source, worker)).unwrap(),
            "version one"
        );
        // An upgraded package updates the copy on the next refresh.
        fs::write(skill_file(&source, worker), "version two").unwrap();
        let packaged = skill_contents(Some(&source)).unwrap();
        assert!(
            refresh(&directory, &packaged, Mode::Refresh)
                .unwrap()
                .changed
        );
        assert_eq!(fs::read_to_string(&installed).unwrap(), "version two");
        assert!(skill_contents(Some(&root.path().join("missing"))).is_err());
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
        refresh(directory, &embedded(), Mode::Install(Changed::Keep)).unwrap();
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
            let outcome = refresh(directory, &embedded(), Mode::Install(Changed::Keep)).unwrap();
            assert_eq!(outcome.installed, [worker]);
            assert_eq!(outcome.kept, [orchestrator]);
            assert_eq!(read(worker), SKILLS[0].1);
            assert_eq!(read(orchestrator), "edited");
        }
        let outcome = refresh(directory, &embedded(), Mode::Install(Changed::Replace)).unwrap();
        assert!(outcome.kept.is_empty());
        assert_eq!(read(orchestrator), SKILLS[1].1);
        // A removed skill is restored.
        fs::remove_file(skill_file(directory, worker)).unwrap();
        refresh(directory, &embedded(), Mode::Install(Changed::Keep)).unwrap();
        assert_eq!(read(worker), SKILLS[0].1);
        // A linked skill directory is the user's, even when forced.
        let elsewhere = tempfile::tempdir().unwrap();
        fs::write(elsewhere.path().join("SKILL.md"), "old").unwrap();
        fs::remove_dir_all(directory.join(worker)).unwrap();
        symlink(elsewhere.path(), directory.join(worker)).unwrap();
        record(directory, &[(worker, Entry::copy(b"old"))]);
        let outcome = refresh(directory, &embedded(), Mode::Install(Changed::Replace)).unwrap();
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
        let packaged = skill_contents(Some(&source)).unwrap();
        refresh(&directory, &packaged, Mode::Install(Changed::Keep)).unwrap();
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
        let outcome = refresh(&directory, &packaged, Mode::Install(Changed::Keep)).unwrap();
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
        let outcome = refresh(&directory, &packaged, Mode::Install(Changed::Keep)).unwrap();
        assert!(outcome.kept.is_empty());
    }

    #[test]
    fn refresh_updates_installed_skills_and_reports_user_changes_once() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path();
        let (worker, orchestrator) = (SKILLS[0].0, SKILLS[1].0);
        let read = |name| fs::read_to_string(skill_file(directory, name)).unwrap();
        // Refreshing never starts an installation.
        let outcome = refresh(directory, &embedded(), Mode::Refresh).unwrap();
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
        let outcome = refresh(directory, &embedded(), Mode::Refresh).unwrap();
        assert!(outcome.changed);
        assert_eq!(outcome.installed, [worker]);
        assert_eq!(outcome.kept, [orchestrator]);
        assert_eq!(read(worker), SKILLS[0].1);
        assert_eq!(read(orchestrator), "edited");
        // A kept skill is reported once by refresh and on every install.
        let outcome = refresh(directory, &embedded(), Mode::Refresh).unwrap();
        assert!(!outcome.changed && outcome.kept.is_empty());
        let outcome = refresh(directory, &embedded(), Mode::Install(Changed::Keep)).unwrap();
        assert_eq!(outcome.kept, [orchestrator]);
        assert_eq!(read(orchestrator), "edited");
        refresh(directory, &embedded(), Mode::Install(Changed::Replace)).unwrap();
        assert_eq!(read(orchestrator), SKILLS[1].1);
        // A kept skill the user made identical to this version stays theirs
        // until an install reclaims it.
        record(directory, &[(orchestrator, Entry::Kept)]);
        let outcome = refresh(directory, &embedded(), Mode::Refresh).unwrap();
        assert_eq!(outcome.installed, [worker]);
        let outcome = refresh(directory, &embedded(), Mode::Install(Changed::Keep)).unwrap();
        assert_eq!(outcome.installed, [worker, orchestrator]);
        assert_eq!(
            Record::load(directory).unwrap().unwrap().skills[orchestrator],
            Entry::copy(SKILLS[1].1.as_bytes())
        );

        // A skill the user removed returns only on install.
        fs::remove_file(skill_file(directory, worker)).unwrap();
        record(directory, &[(worker, Entry::copy(b"old"))]);
        let outcome = refresh(directory, &embedded(), Mode::Refresh).unwrap();
        assert!(outcome.kept.is_empty() && !skill_file(directory, worker).exists());
        refresh(directory, &embedded(), Mode::Refresh).unwrap();
        assert!(!skill_file(directory, worker).exists());
        refresh(directory, &embedded(), Mode::Install(Changed::Keep)).unwrap();
        assert_eq!(read(worker), SKILLS[0].1);

        // Removing every Shoal skill stops refreshes there.
        for (name, _) in SKILLS {
            fs::remove_dir_all(directory.join(name)).unwrap();
        }
        record(directory, &[("shoal-new", Entry::Kept)]);
        refresh(directory, &embedded(), Mode::Refresh).unwrap();
        assert!(!directory.join(worker).exists());
    }

    #[test]
    fn diff_compares_the_installed_skill_with_this_version() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("SKILL.md");
        fs::write(&path, "edited\n").unwrap();
        show_diff(&path, "edited\n").unwrap();
        show_diff(&path, "bundled\n").unwrap();
        assert!(show_diff(&root.path().join("missing"), "bundled\n").is_err());
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
