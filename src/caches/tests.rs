use super::*;
use crate::config::repo::CacheScope;

#[test]
fn only_a_cache_directory_marked_for_the_repository_is_owned() {
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::new(root.path(), "repo");
    fs::create_dir(layout.root()).unwrap();
    assert_eq!(layout.owned("go"), None, "unmarked");
    layout.remove().unwrap();
    fs::remove_dir(layout.root()).unwrap();
    Layout::new(root.path(), "other")
        .prepare(&configured(&["go"]))
        .unwrap();
    assert_eq!(layout.owned("go"), None, "another repository's");
    Layout::new(root.path(), "other").remove().unwrap();
    let outside = root.path().join("outside");
    fs::create_dir(&outside).unwrap();
    layout.prepare(&configured(&["go"])).unwrap();
    let shared = root.path().join(".caches/shared");
    assert_eq!(layout.owned("go"), Some(shared.join("go")));
    std::os::unix::fs::symlink(&outside, shared.join("linked")).unwrap();
    assert_eq!(layout.owned("linked"), None, "a symlinked cache");
    fs::rename(&shared, root.path().join("moved")).unwrap();
    std::os::unix::fs::symlink(root.path().join("moved"), &shared).unwrap();
    assert_eq!(layout.owned("go"), None, "a symlinked shared directory");
}

fn configured(names: &[&str]) -> BTreeMap<String, Cache> {
    names
        .iter()
        .map(|name| {
            (
                (*name).to_owned(),
                Cache {
                    scope: CacheScope::Shared,
                },
            )
        })
        .collect()
}

#[test]
fn caches_are_created_emptied_and_removed_only_when_owned() {
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::new(root.path(), "repo");
    let directory = root.path().join(".caches");
    assert!(layout.prepare(&BTreeMap::new()).unwrap().is_empty());
    assert!(!directory.exists());

    let directories = layout.prepare(&configured(&["go", "sccache"])).unwrap();
    assert_eq!(directories["go"], root.path().join(".caches/shared/go"));
    fs::write(directories["go"].join("entry"), "cached").unwrap();
    let dropped = directory.join(SHARED).join("dropped");
    fs::create_dir(&dropped).unwrap();

    assert_eq!(layout.list().unwrap(), ["dropped", "go", "sccache"]);
    let moved = layout.move_aside().unwrap();
    assert_eq!(moved, ["dropped", "go", "sccache"]);
    assert!(layout.list().unwrap().is_empty());
    assert!(!dropped.exists());
    assert!(directory.join(TRASH).is_dir());
    layout.purge().unwrap();
    assert!(!directory.join(TRASH).exists());
    let directories = layout.prepare(&configured(&["go", "sccache"])).unwrap();

    // Another repository's directory is never used or deleted.
    let other = Layout::new(root.path(), "other");
    assert!(other.prepare(&configured(&["go"])).is_err());
    assert!(other.move_aside().is_err());
    other.remove().unwrap();
    assert!(directories["sccache"].is_dir());

    layout.remove().unwrap();
    assert!(!directory.exists());
    assert!(layout.move_aside().unwrap().is_empty());
}

#[test]
fn cache_names_are_lowercase_identifiers() {
    validate(&configured(&["go", "pnpm_store", "gradle-home"])).unwrap();
    for name in ["", "Go", ".go", "../go", "go/build"] {
        assert!(validate(&configured(&[name])).is_err(), "{name:?}");
    }
}
