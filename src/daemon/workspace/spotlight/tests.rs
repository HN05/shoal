use super::*;

#[test]
fn spotlight_skips_noindex_and_hidden_names_anywhere_in_a_path() {
    assert!(skips(Path::new("/Volumes/SSD/.shoal.noindex/repo")));
    assert!(skips(Path::new("/Users/me/shoal.noindex")));
    assert!(skips(Path::new("/Users/me/.cache/shoal")));
    assert!(!skips(Path::new("/Users/me/shoal")));
    assert!(!skips(Path::new("/Users/me/noindex/shoal")));
}

#[test]
fn the_excluded_directory_is_a_hidden_noindex_sibling() {
    assert_eq!(
        excluded_directory(Path::new("/Volumes/SSD/shoal")).unwrap(),
        Path::new("/Volumes/SSD/.shoal.noindex")
    );
}

#[test]
fn an_existing_root_is_kept_as_found() {
    let parent = parent();
    let root = parent.join("shoal");
    fs::create_dir_all(root.join("repo")).unwrap();
    assert_eq!(prepare(&root, Layout::AsFound), root);
    assert!(fs::symlink_metadata(&root).unwrap().is_dir());

    let linked = parent.join("linked");
    std::os::unix::fs::symlink("shoal", &linked).unwrap();
    assert_eq!(prepare(&linked, Layout::AsFound), root);
}

#[test]
fn a_regular_root_is_the_configured_directory() {
    let parent = parent();
    let root = parent.join("shoal");
    assert_eq!(prepare(&root, Layout::Regular), root);
    assert!(fs::symlink_metadata(&root).unwrap().is_dir());
}

#[test]
fn an_excluded_root_links_a_missing_or_empty_directory() {
    let parent = parent();
    let hidden = parent.join(".shoal.noindex");
    let missing = parent.join("shoal");
    assert_eq!(prepare(&missing, Layout::Excluded), hidden);
    assert_eq!(
        fs::read_link(&missing).unwrap(),
        Path::new(".shoal.noindex")
    );
    // Preparing it again leaves the link.
    assert_eq!(prepare(&missing, Layout::Excluded), hidden);
    assert_eq!(prepare(&missing, Layout::AsFound), hidden);

    let empty = parent.join("work");
    fs::create_dir(&empty).unwrap();
    fs::write(empty.join(".DS_Store"), "").unwrap();
    assert_eq!(
        prepare(&empty, Layout::Excluded),
        parent.join(".work.noindex")
    );
    assert_eq!(fs::read_link(&empty).unwrap(), Path::new(".work.noindex"));
}

#[test]
fn an_excluded_root_holding_files_stays_a_directory_until_it_is_empty() {
    let parent = parent();
    let root = parent.join("shoal");
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_eq!(
        prepare(&root, Layout::Excluded),
        parent.join(".shoal.noindex")
    );
    assert!(repo.is_dir());
    fs::remove_dir(&repo).unwrap();
    prepare(&root, Layout::Excluded);
    assert_eq!(fs::read_link(&root).unwrap(), Path::new(".shoal.noindex"));
}

#[test]
fn a_root_spotlight_already_skips_keeps_its_place() {
    let parent = parent();
    let root = parent.join("shoal.noindex");
    assert_eq!(prepare(&root, Layout::Excluded), root);
    assert!(fs::symlink_metadata(&root).unwrap().is_dir());
}

/// A canonical temporary directory whose own name Spotlight does not skip,
/// unlike `tempfile`'s default `.tmp` prefix, kept until the test ends.
fn parent() -> Parent {
    let temp = tempfile::Builder::new()
        .prefix("spotlight")
        .tempdir()
        .unwrap();
    let path = fs::canonicalize(temp.path()).unwrap();
    Parent { _temp: temp, path }
}

struct Parent {
    _temp: tempfile::TempDir,
    path: PathBuf,
}

impl Parent {
    fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

/// Plan and apply `root` in `layout`, returning where repositories go.
fn prepare(root: &Path, layout: Layout) -> PathBuf {
    let plan = plan(root, layout).unwrap();
    plan.apply().unwrap();
    plan.destination().unwrap()
}
