use shoal_daemon_base::callers::Access;
use shoal_daemon_base::callers::Caller;
use shoal_forge_types::items::{
    action::Action,
    review::{NewReview, ReviewVerdict},
};

use super::*;

#[test]
fn a_scoped_verdict_is_the_space_s_own_post_and_the_user_s_is_not() {
    let approval = Action::Review(NewReview {
        verdict: ReviewVerdict::Approve,
        body: "Looks right".into(),
        comments: Vec::new(),
        commit: None,
    });
    let posted = || vec!["review-1".to_owned()];
    let scoped = Caller {
        space_id: "space".into(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };
    assert_eq!(own_posts(&approval, Some(&scoped), posted()), posted());
    assert!(own_posts(&approval, None, posted()).is_empty());
}

#[tokio::test]
async fn a_merge_naming_no_method_takes_the_configured_one() {
    use shoal_forge_types::items::action::MergeMethod;
    let root = tempfile::tempdir().unwrap();
    let repo: shoal_protocol::model::Repository = serde_json::from_value(serde_json::json!({
        "id": "repo",
        "path": root.path(),
        "source": root.path(),
        "last_used": 0,
        "workspaces_dir": root.path(),
    }))
    .unwrap();
    let daemon = crate::stand_in::StandIn {
        repositories: vec![repo.clone()],
        config: Some(shoal_core::test_support::global_config(
            "[merge]\nmethod = \"squash\"\n",
        )),
        repository_files: [(repo.id.clone(), "[merge]\nmethod = \"rebase\"\n".into())].into(),
        ..Default::default()
    };
    let merge = |method| Action::Merge {
        method,
        message: None,
        delete_branch: false,
        head: None,
    };
    let method = async |place: Option<&Place>, mut action| {
        configure_merge(&daemon, place, &mut action).await.unwrap();
        match action {
            Action::Merge { method, .. } => method,
            other => panic!("not a merge: {other:?}"),
        }
    };
    let checkout = Place::Checkout(repo);
    assert_eq!(
        method(Some(&checkout), merge(None)).await,
        Some(MergeMethod::Rebase)
    );
    // Another repository than the request's own takes the global config's.
    assert_eq!(method(None, merge(None)).await, Some(MergeMethod::Squash));
    assert_eq!(
        method(Some(&checkout), merge(Some(MergeMethod::Merge))).await,
        Some(MergeMethod::Merge)
    );
}
