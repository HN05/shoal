use super::*;

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
async fn status_reports_git_checks_on_a_stalled_volume_as_unavailable() {
    let (temp, manager) = crate::test_support::manager().await;
    let checkout = crate::test_support::repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "stalled".into(), None, None, None)
        .await
        .unwrap();
    let status = manager.workspace_status(&workspace.id).await.unwrap();
    assert!(status.diff.is_some(), "{:?}", status.diff_error);

    tokio::time::pause();
    let stalled = manager
        .check_volume(&workspace.path, std::future::pending::<Result<()>>())
        .await;
    tokio::time::resume();
    assert!(stalled.is_err());

    let status = manager.workspace_status(&workspace.id).await.unwrap();
    assert!(status.diff.is_none());
    let stall = "stalled; a macOS privacy prompt may be waiting for an answer";
    let error = status.diff_error.unwrap();
    assert!(error.contains(stall), "{error}");
    let crate::model::CleanupReason::InspectionFailed { error } = status.cleanup_reason else {
        panic!("expected a failed inspection: {:?}", status.cleanup_reason);
    };
    assert!(error.contains(stall), "{error}");
}
