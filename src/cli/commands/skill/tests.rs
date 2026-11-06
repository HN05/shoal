use super::*;
use std::os::unix::fs::symlink;

/// A second bundled skill, so one skill can be installed while another is kept.
const OTHER: &str = "shoal-other";

fn embedded() -> Contents {
    skill_contents(None).unwrap()
}

/// The embedded skills and [`OTHER`], holding `other`.
fn with_other(mut skills: Contents, other: &str) -> Contents {
    skills.push((OTHER, other.to_owned()));
    skills
}

fn bundled() -> Contents {
    with_other(embedded(), "other")
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
    fs::create_dir_all(&incomplete).unwrap();
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
fn packaged_skills_link_sits_beside_an_app_bundle() {
    let root = tempfile::tempdir().unwrap();
    let macos = root.path().join("Shoal.app/Contents/MacOS");
    let source = root.path().join("skills");
    fs::create_dir_all(&macos).unwrap();
    fs::write(macos.join("shoal"), "binary").unwrap();
    for (name, _) in SKILLS {
        fs::create_dir_all(source.join(name)).unwrap();
        fs::write(skill_file(&source, name), "packaged").unwrap();
    }
    symlink(&source, root.path().join("shoal-skills")).unwrap();
    let launcher = root.path().join("shoal");
    symlink(macos.join("shoal"), &launcher).unwrap();
    assert_eq!(packaged_source(None, &launcher).unwrap(), Some(source));
    // Only an app bundle's executable looks outside its own directory.
    let plain = root.path().join("Plain/Contents/MacOS");
    fs::create_dir_all(&plain).unwrap();
    fs::write(plain.join("shoal"), "binary").unwrap();
    assert_eq!(packaged_source(None, &plain.join("shoal")).unwrap(), None);
}

#[test]
fn packaged_skills_replace_recorded_links_with_copies_that_follow_upgrades() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("skills");
    let source = root.path().join("package");
    let (shoal, other) = (SKILLS[0].0, OTHER);
    for name in [shoal, other] {
        fs::create_dir_all(source.join(name)).unwrap();
        fs::write(skill_file(&source, name), "version one").unwrap();
        fs::create_dir_all(directory.join(name)).unwrap();
    }
    fs::write(directory.join(shoal).join("notes.md"), "keep").unwrap();
    // An earlier version linked both skills; the user since repointed one.
    let personal = root.path().join("personal.md");
    fs::write(&personal, "personal").unwrap();
    symlink(skill_file(&source, shoal), skill_file(&directory, shoal)).unwrap();
    symlink(&personal, skill_file(&directory, other)).unwrap();
    record(
        &directory,
        &[
            (shoal, Entry::Link(skill_file(&source, shoal))),
            (other, Entry::Link(skill_file(&source, other))),
        ],
    );
    let packaged = with_other(skill_contents(Some(&source)).unwrap(), "version one");
    let outcome = refresh(&directory, &packaged, Mode::Refresh).unwrap();
    assert_eq!(outcome.installed, [shoal]);
    assert_eq!(outcome.kept, [other]);
    let installed = skill_file(&directory, shoal);
    assert!(!installed.is_symlink());
    assert_eq!(fs::read_to_string(&installed).unwrap(), "version one");
    assert_eq!(
        fs::read_link(skill_file(&directory, other)).unwrap(),
        personal
    );
    assert_eq!(
        fs::read_to_string(directory.join(shoal).join("notes.md")).unwrap(),
        "keep"
    );
    assert_eq!(
        fs::read_to_string(skill_file(&source, shoal)).unwrap(),
        "version one"
    );
    // An upgraded package updates the copy on the next refresh.
    fs::write(skill_file(&source, shoal), "version two").unwrap();
    let packaged = with_other(skill_contents(Some(&source)).unwrap(), "version one");
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
    let (shoal, other) = (SKILLS[0].0, OTHER);
    let read = |name| fs::read_to_string(skill_file(directory, name)).unwrap();
    // Skills from before the record existed are Shoal's.
    for name in [shoal, other] {
        fs::create_dir_all(directory.join(name)).unwrap();
        fs::write(skill_file(directory, name), "old").unwrap();
    }
    refresh(directory, &bundled(), Mode::Install(Changed::Keep)).unwrap();
    assert_eq!(read(shoal), SKILLS[0].1);
    // A recorded copy from an earlier version is replaced; an edited one is kept.
    fs::write(skill_file(directory, shoal), "old").unwrap();
    fs::write(skill_file(directory, other), "edited").unwrap();
    record(
        directory,
        &[(shoal, Entry::copy(b"old")), (other, Entry::copy(b"old"))],
    );
    for _ in 0..2 {
        let outcome = refresh(directory, &bundled(), Mode::Install(Changed::Keep)).unwrap();
        assert_eq!(outcome.installed, [shoal]);
        assert_eq!(outcome.kept, [other]);
        assert_eq!(read(shoal), SKILLS[0].1);
        assert_eq!(read(other), "edited");
    }
    let outcome = refresh(directory, &bundled(), Mode::Install(Changed::Replace)).unwrap();
    assert!(outcome.kept.is_empty());
    assert_eq!(read(other), "other");
    // A removed skill is restored.
    fs::remove_file(skill_file(directory, shoal)).unwrap();
    refresh(directory, &bundled(), Mode::Install(Changed::Keep)).unwrap();
    assert_eq!(read(shoal), SKILLS[0].1);
    // A linked skill directory is the user's, even when forced.
    let elsewhere = tempfile::tempdir().unwrap();
    fs::write(elsewhere.path().join("SKILL.md"), "old").unwrap();
    fs::remove_dir_all(directory.join(shoal)).unwrap();
    symlink(elsewhere.path(), directory.join(shoal)).unwrap();
    record(directory, &[(shoal, Entry::copy(b"old"))]);
    let outcome = refresh(directory, &bundled(), Mode::Install(Changed::Replace)).unwrap();
    assert_eq!(outcome.kept, [shoal]);
    assert_eq!(read(shoal), "old");
}

#[test]
fn an_unrecorded_skill_in_a_recorded_directory_is_the_users() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path();
    let name = SKILLS[0].0;
    let read = || fs::read_to_string(skill_file(directory, name)).unwrap();
    fs::create_dir_all(directory.join(name)).unwrap();
    fs::write(skill_file(directory, name), "mine").unwrap();
    record(directory, &[("retired", Entry::Kept)]);
    let outcome = refresh(directory, &embedded(), Mode::Refresh).unwrap();
    assert_eq!(outcome.kept, [name]);
    assert_eq!(read(), "mine");
    let outcome = refresh(directory, &embedded(), Mode::Install(Changed::Keep)).unwrap();
    assert_eq!(outcome.kept, [name]);
    assert_eq!(read(), "mine");
    refresh(directory, &embedded(), Mode::Install(Changed::Replace)).unwrap();
    assert_eq!(read(), SKILLS[0].1);
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
    assert_eq!(names, [SKILLS[0].0]);
    // A retired skill is reported once, then belongs to the user.
    let outcome = refresh(&directory, &packaged, Mode::Install(Changed::Keep)).unwrap();
    assert!(outcome.kept.is_empty());
}

#[test]
fn refresh_updates_installed_skills_and_reports_user_changes_once() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path();
    let (shoal, other) = (SKILLS[0].0, OTHER);
    let read = |name| fs::read_to_string(skill_file(directory, name)).unwrap();
    // Refreshing never starts an installation.
    let outcome = refresh(directory, &bundled(), Mode::Refresh).unwrap();
    assert!(!outcome.changed && outcome.installed.is_empty());
    assert_eq!(fs::read_dir(directory).unwrap().count(), 0);

    // An upgrade replaces what an earlier version wrote.
    for name in [shoal, other] {
        fs::create_dir_all(directory.join(name)).unwrap();
        fs::write(skill_file(directory, name), "old").unwrap();
    }
    record(directory, &[(shoal, Entry::copy(b"old"))]);
    fs::write(skill_file(directory, other), "edited").unwrap();
    record(directory, &[(other, Entry::copy(b"old"))]);
    let outcome = refresh(directory, &bundled(), Mode::Refresh).unwrap();
    assert!(outcome.changed);
    assert_eq!(outcome.installed, [shoal]);
    assert_eq!(outcome.kept, [other]);
    assert_eq!(read(shoal), SKILLS[0].1);
    assert_eq!(read(other), "edited");
    // A kept skill is reported once by refresh and on every install.
    let outcome = refresh(directory, &bundled(), Mode::Refresh).unwrap();
    assert!(!outcome.changed && outcome.kept.is_empty());
    let outcome = refresh(directory, &bundled(), Mode::Install(Changed::Keep)).unwrap();
    assert_eq!(outcome.kept, [other]);
    assert_eq!(read(other), "edited");
    refresh(directory, &bundled(), Mode::Install(Changed::Replace)).unwrap();
    assert_eq!(read(other), "other");
    // A kept skill the user made identical to this version stays theirs
    // until an install reclaims it.
    record(directory, &[(other, Entry::Kept)]);
    let outcome = refresh(directory, &bundled(), Mode::Refresh).unwrap();
    assert_eq!(outcome.installed, [shoal]);
    let outcome = refresh(directory, &bundled(), Mode::Install(Changed::Keep)).unwrap();
    assert_eq!(outcome.installed, [shoal, other]);
    assert_eq!(
        Record::load(directory).unwrap().unwrap().skills[other],
        Entry::copy(b"other")
    );

    // A skill the user removed returns only on install.
    fs::remove_file(skill_file(directory, shoal)).unwrap();
    record(directory, &[(shoal, Entry::copy(b"old"))]);
    let outcome = refresh(directory, &bundled(), Mode::Refresh).unwrap();
    assert!(outcome.kept.is_empty() && !skill_file(directory, shoal).exists());
    refresh(directory, &bundled(), Mode::Refresh).unwrap();
    assert!(!skill_file(directory, shoal).exists());
    refresh(directory, &bundled(), Mode::Install(Changed::Keep)).unwrap();
    assert_eq!(read(shoal), SKILLS[0].1);

    // Removing every Shoal skill stops refreshes there.
    for name in [shoal, other] {
        fs::remove_dir_all(directory.join(name)).unwrap();
    }
    record(directory, &[("shoal-new", Entry::Kept)]);
    refresh(directory, &bundled(), Mode::Refresh).unwrap();
    assert!(!directory.join(shoal).exists());
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
fn a_refresh_replaces_the_skills_installed_before_the_record() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path();
    for name in UNRECORDED {
        fs::create_dir_all(directory.join(name)).unwrap();
        fs::write(skill_file(directory, name), "earlier").unwrap();
    }
    fs::write(directory.join(UNRECORDED[0]).join("notes.md"), "keep").unwrap();
    let outcome = refresh(directory, &embedded(), Mode::Refresh).unwrap();
    assert!(outcome.changed);
    assert_eq!(outcome.installed, [SKILLS[0].0]);
    assert!(!skill_file(directory, UNRECORDED[0]).exists());
    assert!(directory.join(UNRECORDED[0]).join("notes.md").exists());
    assert!(!directory.join(UNRECORDED[1]).exists());
    // Once the record exists, an unrecorded skill of that name is the user's.
    fs::create_dir(directory.join(UNRECORDED[1])).unwrap();
    fs::write(skill_file(directory, UNRECORDED[1]), "mine").unwrap();
    refresh(directory, &embedded(), Mode::Install(Changed::Keep)).unwrap();
    assert!(skill_file(directory, UNRECORDED[1]).exists());
}

#[test]
fn retired_skill_is_removed_without_other_files() {
    let root = tempfile::tempdir().unwrap();
    let retired = root.path().join("retired");
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
