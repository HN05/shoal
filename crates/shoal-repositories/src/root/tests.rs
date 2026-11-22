use std::path::PathBuf;

use super::{ConfiguredRoot, Recorded, prepare_root, reserve_directory};
use crate::spotlight::Layout;

/// A canonical temporary directory whose path Spotlight does not skip, and
/// an excluded `root_dir` named `shoal` in it. It lies in `/tmp`, since
/// `$TMPDIR` can lie in a hidden directory such as a Shoal workspace's.
pub(crate) fn excluded_root() -> (tempfile::TempDir, PathBuf, ConfiguredRoot) {
    let temp = tempfile::Builder::new()
        .prefix("registry")
        .tempdir_in("/tmp")
        .unwrap();
    let parent = std::fs::canonicalize(temp.path()).unwrap();
    let root = ConfiguredRoot {
        path: parent.join("shoal"),
        layout: Layout::Excluded,
    };
    (temp, parent, root)
}

pub(crate) fn nothing() -> Recorded<'static> {
    Recorded {
        repositories: &[],
        workspaces: &[],
    }
}

#[tokio::test]
async fn an_excluded_root_refuses_a_sibling_that_resolves_into_state() {
    let (_temp, parent, root) = excluded_root();
    let state = parent.join("state");
    std::fs::create_dir(&state).unwrap();
    std::os::unix::fs::symlink(&state, parent.join(".shoal.noindex")).unwrap();
    let prepared = prepare_root(&root, &state, &nothing(), None).await;
    assert!(prepared.is_err());
    assert!(!parent.join("shoal").exists());
}

#[tokio::test]
async fn an_excluded_root_refuses_a_workspace_at_its_excluded_directory() {
    let (_temp, parent, root) = excluded_root();
    let placed = parent.join(".shoal.noindex");
    std::fs::create_dir(&placed).unwrap();
    let workspace = shoal_protocol::test_support::workspace("placed", "repo", "placed", &placed);
    let recorded = Recorded {
        repositories: &[],
        workspaces: &[workspace],
    };
    let prepared = prepare_root(&root, &parent.join("state"), &recorded, None).await;
    assert!(prepared.is_err());
    assert!(std::fs::symlink_metadata(&root.path).is_err());
}

#[tokio::test]
async fn an_excluded_root_refuses_a_recorded_repository_directory() {
    let (_temp, parent, root) = excluded_root();
    std::fs::create_dir(&root.path).unwrap();
    let recorded = shoal_protocol::model::Repository {
        id: "recorded".into(),
        path: parent.join("elsewhere/checkout"),
        source: "https://example.invalid/recorded.git".into(),
        last_used: 1,
        name: None,
        workspaces_dir: root.path.clone(),
        contribution: false,
    };
    let prepared = prepare_root(
        &root,
        &parent.join("state"),
        &Recorded {
            repositories: &[recorded],
            workspaces: &[],
        },
        None,
    )
    .await;
    assert!(prepared.is_err());
    assert!(std::fs::symlink_metadata(&root.path).unwrap().is_dir());
}

#[test]
fn the_destination_avoids_a_missing_checkout_recorded_inside_it() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let clone = shoal_protocol::model::Repository {
        id: "clone".into(),
        path: root.join("project/.checkout"),
        source: "https://example.invalid/clone.git".into(),
        last_used: 1,
        name: None,
        workspaces_dir: root.join("elsewhere"),
        contribution: false,
    };
    let reserved = reserve_directory(&root, "project", std::slice::from_ref(&clone), &[])?;
    assert_eq!(reserved, root.join("project-2"));

    // A root inside a recorded directory has no free name at all.
    let nested = root.join("elsewhere/nested");
    assert!(reserve_directory(&nested, "project", &[clone], &[]).is_err());
    Ok(())
}
