use super::*;
use crate::client::space_names::SpaceSelector;

#[test]
fn numstat_counts_text_and_binary_files() {
    let summary = parse_numstat("2\t1\tfirst\n10\t0\tsecond\n-\t-\timage.png\n").unwrap();
    assert_eq!(summary.files_changed, 3);
    assert_eq!(summary.insertions, 12);
    assert_eq!(summary.deletions, 1);
}

#[test]
fn numstat_rejects_incomplete_records() {
    assert!(parse_numstat("1\t2\n").is_err());
}

#[tokio::test]
async fn status_reports_git_checks_that_stall_as_unavailable() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let checkout = crate::daemon::test_support::repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "stalled".into(), None, None, None)
        .await
        .unwrap();
    // A saved conflict check makes status read HEAD.
    manager
        .check_conflicts(&SpaceSelector::id(&workspace.id), None)
        .await
        .unwrap();
    let status = manager
        .worktree_status(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap()
        .0;
    assert!(status.diff.is_some(), "{:?}", status.diff_error);
    assert!(status.changed_on_both_sides.is_some());

    tokio::time::pause();
    let stalled = manager
        .check_access(&workspace.path, std::future::pending::<Result<()>>())
        .await;
    tokio::time::resume();
    assert!(stalled.is_err());

    let status = manager
        .worktree_status(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap()
        .0;
    assert!(status.diff.is_none());
    assert!(status.changed_on_both_sides.is_none());
    assert!(status.diff_error.is_some());
    assert!(
        matches!(
            status.cleanup_reason,
            crate::model::CleanupReason::InspectionFailed { .. }
        ),
        "{:?}",
        status.cleanup_reason
    );
}
