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
