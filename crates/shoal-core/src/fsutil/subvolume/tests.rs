use super::*;
use crate::test_support::subvolumes::{Call, FakeSubvolumes};

fn tree(root: &Path, outputs: &[&str]) {
    for output in outputs {
        fs::create_dir_all(root.join(output)).unwrap();
        fs::write(root.join(output).join("file"), "built").unwrap();
    }
}

#[test]
fn removal_destroys_output_subvolumes_deepest_first_then_deletes_the_rest() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("worktree");
    tree(
        &root,
        &["target", "target/nested", "web/node_modules", "plain"],
    );
    let volumes = FakeSubvolumes::destroying();
    for output in ["target", "target/nested", "web/node_modules"] {
        volumes.add_root(&root.join(output));
    }
    let outputs = [
        "target",
        "web/node_modules",
        "target/nested",
        "plain",
        "../x",
    ]
    .map(String::from);

    remove_tree(&volumes, &root, &outputs).unwrap();

    assert_eq!(
        volumes.calls(),
        [
            Call::Destroy(root.join("web/node_modules")),
            Call::Destroy(root.join("target/nested")),
            Call::Destroy(root.join("target")),
        ]
    );
    assert!(root.symlink_metadata().is_err());
}

#[test]
fn removal_deletes_subvolumes_it_may_not_destroy() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("spare");
    tree(&root, &["target"]);
    let volumes = FakeSubvolumes::default();
    volumes.add_root(&root);
    volumes.add_root(&root.join("target"));

    remove_tree(&volumes, &root, &["target".into()]).unwrap();

    assert_eq!(
        volumes.calls(),
        [
            Call::Destroy(root.join("target")),
            Call::Destroy(root.clone())
        ]
    );
    assert!(root.symlink_metadata().is_err());
}

#[test]
fn a_filesystem_without_subvolumes_fails_the_probe() {
    let temp = tempfile::tempdir().unwrap();
    let probe = temp.path().join("probe");
    assert!(!removable_at(&FakeSubvolumes::unsupported(), &probe));
    assert!(removable_at(&FakeSubvolumes::default(), &probe));
    assert!(probe.symlink_metadata().is_err());
}

/// The real ioctls, where the temporary directory is on btrfs.
#[test]
fn btrfs_snapshots_are_writable_copies_that_removal_deletes() {
    let temp = tempfile::tempdir().unwrap();
    if !removable_at(&Btrfs, &temp.path().join("probe")) {
        return;
    }
    let source = temp.path().join("source");
    Btrfs.create(&source).unwrap();
    fs::write(source.join("file"), "built").unwrap();
    assert!(Btrfs.is_root(&source));
    assert!(!Btrfs.is_root(temp.path()));

    let copy = temp.path().join("copy");
    Btrfs.snapshot(&source, &copy).unwrap();
    fs::write(copy.join("file"), "changed").unwrap();
    assert_eq!(fs::read_to_string(source.join("file")).unwrap(), "built");
    assert!(Btrfs.is_root(&copy));

    remove_tree(&Btrfs, temp.path(), &["source".into(), "copy".into()]).unwrap();
    assert!(temp.path().symlink_metadata().is_err());
}
