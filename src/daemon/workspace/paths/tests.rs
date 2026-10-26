use super::*;
use std::os::unix::fs::symlink;

#[test]
fn missing_tail_normalizes_components_without_creating_them() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = fs::canonicalize(temp.path())?;
    fs::create_dir_all(root.join("target/child"))?;
    symlink(root.join("target/child"), root.join("alias"))?;
    for (input, expected) in [
        ("missing/leaf", "missing/leaf"),
        ("missing/../target/./child", "target/child"),
        ("alias/../new/leaf", "target/new/leaf"),
        ("alias/../child", "target/child"),
        ("missing/../alias/../new", "target/new"),
    ] {
        assert_eq!(
            canonical_with_missing_tail(&root.join(input))?,
            root.join(expected)
        );
    }
    assert!(!root.join("missing").exists());
    assert!(!root.join("target/new").exists());
    assert_eq!(
        canonical_with_missing_tail(Path::new("/../../"))?,
        Path::new("/")
    );
    assert!(canonical_with_missing_tail(Path::new("relative/path")).is_err());
    Ok(())
}

#[test]
fn missing_tail_rejects_dangling_links_and_non_directory_ancestors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    symlink(root.join("missing"), root.join("dangling"))?;
    fs::write(root.join("file"), "contents")?;
    for path in ["dangling", "dangling/leaf", "dangling/../leaf", "file/leaf"] {
        assert!(
            canonical_with_missing_tail(&root.join(path)).is_err(),
            "{path}"
        );
    }
    Ok(())
}

#[test]
fn identity_policies_distinguish_missing_files_and_symlinks() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = fs::canonicalize(temp.path())?;
    let directory = root.join("directory");
    fs::create_dir(&directory)?;
    let identity = real_directory_identity(&directory)?;
    assert!(identity.is_some());

    let alias = root.join("alias");
    symlink(&directory, &alias)?;
    assert!(real_directory_identity(&alias).is_err());
    fs::create_dir(directory.join("child"))?;
    assert!(real_directory_identity(&alias.join("child")).is_err());

    let missing = root.join("missing");
    assert_eq!(real_directory_identity(&missing)?, None);
    let dangling = root.join("dangling");
    symlink(&missing, &dangling)?;
    assert!(real_directory_identity(&dangling).is_err());

    let file = root.join("file");
    fs::write(&file, "contents")?;
    assert!(real_directory_identity(&file).is_err());
    Ok(())
}

#[test]
fn identity_survives_rename_but_detects_replacement() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = fs::canonicalize(temp.path())?;
    let directory = root.join("directory");
    fs::create_dir(&directory)?;
    let identity = real_directory_identity(&directory)?;
    let moved = root.join("moved");
    fs::rename(&directory, &moved)?;
    assert_eq!(real_directory_identity(&moved)?, identity);
    fs::create_dir(&directory)?;
    assert_ne!(real_directory_identity(&directory)?, identity);
    Ok(())
}

#[test]
fn parent_only_resolves_ancestors_but_preserves_the_leaf() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = fs::canonicalize(temp.path())?;
    let alias = root.join("alias");
    symlink(&root, &alias)?;
    assert_eq!(
        canonical_parent_with_missing(&alias.join("missing"))?,
        root.join("missing")
    );
    assert_eq!(canonical_parent_with_missing(&alias)?, alias);
    Ok(())
}

#[test]
fn parent_with_missing_normalizes_deleted_ancestors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = fs::canonicalize(temp.path())?;
    let path = root.join("removed/parent/workspace");
    assert_eq!(canonical_parent_with_missing(&path)?, path);
    fs::create_dir_all(root.join("removed/parent"))?;
    assert_eq!(canonical_parent_with_missing(&path)?, path);
    Ok(())
}

#[test]
fn protected_directories_reject_ancestors_and_resolve_aliases() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = fs::canonicalize(temp.path())?;
    let home = root.join("home");
    let state = root.join("state");
    fs::create_dir(&home)?;
    fs::create_dir(&state)?;
    let alias = root.join("alias");
    symlink(&home, &alias)?;
    let paths = Paths {
        home: alias,
        state: state.clone(),
        socket: state.join("daemon.sock"),
    };
    for path in [&root, &home, &state] {
        assert!(contains_protected_directory(path, &paths)?);
    }
    for path in [
        home.join("child"),
        state.join("child"),
        root.join("home-other"),
    ] {
        assert!(!contains_protected_directory(&path, &paths)?);
    }
    fs::remove_dir(&state)?;
    assert!(contains_protected_directory(&root.join("other"), &paths).is_err());
    Ok(())
}

#[test]
fn recorded_worktree_path_matches_canonical_parent_and_original() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = fs::canonicalize(temp.path())?;
    let alias = root.join("alias");
    symlink(&root, &alias)?;
    let recorded = alias.join("missing");

    assert!(matches_recorded_worktree_path(
        &recorded,
        &root.join("missing")
    )?);
    assert!(matches_recorded_worktree_path(&recorded, &recorded)?);
    assert!(!matches_recorded_worktree_path(
        &recorded,
        &root.join("other")
    )?);
    Ok(())
}
