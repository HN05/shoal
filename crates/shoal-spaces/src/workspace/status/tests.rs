use crate::stand_in::Fixture;
use crate::workspace::status::*;
use anyhow::Result;
use shoal_store::test_support::seeds;

use shoal_protocol::space_names::SpaceSelector;

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
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let checkout = crate::stand_in::repository(temp.path(), "repo");
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
        .spaces
        .check_conflicts(&*manager, &SpaceSelector::id(&workspace.id), None)
        .await
        .unwrap();
    let status = manager
        .spaces
        .worktree_status(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert!(status.diff.is_some(), "{:?}", status.diff_error);
    assert!(status.changed_on_both_sides.is_some());

    tokio::time::pause();
    let stalled = manager
        .spaces
        .file_access
        .checked(
            &manager.spaces.notifications,
            &workspace.path,
            std::future::pending::<Result<()>>(),
        )
        .await;
    tokio::time::resume();
    assert!(stalled.is_err());

    let status = manager
        .spaces
        .worktree_status(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert!(status.diff.is_none());
    assert!(status.changed_on_both_sides.is_none());
    assert!(status.diff_error.is_some());
    let space = crate::model::Space::from(workspace.clone());
    let reason = manager.spaces.cleanup_reason(&*manager, &space).await;
    assert!(
        matches!(reason, crate::model::CleanupReason::InspectionFailed { .. }),
        "{reason:?}"
    );
}

#[test]
fn setup_finished_reads_the_workspace_s_record() -> Result<()> {
    let db = shoal_store::test_support::migrated()?;
    for id in ["finished", "pending"] {
        seeds::ready_workspace(&db, "repo", id, id)?;
    }
    db.execute(
        "UPDATE workspaces SET setup_finished=1 WHERE id='finished'",
        [],
    )?;
    assert!(setup_finished(&db, &"finished".into())?);
    assert!(!setup_finished(&db, &"pending".into())?);
    Ok(())
}
