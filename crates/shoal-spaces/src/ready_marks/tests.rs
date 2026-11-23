use crate::forge::items::link::Selection;
use crate::model::ReviewMark;
use crate::notifications::AlertKind;
use crate::stand_in::Fixture;
use shoal_core::test_support::{install_test_script, repository};

use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn post_ready_hook_receives_marks_and_its_failure_keeps_them() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "hooked".into(), None, None, None)
        .await
        .unwrap();
    let record = temp.path().join("marks");
    let hook = temp.path().join("post-ready");
    install_test_script(
        &hook,
        format!(
            "#!/bin/sh\nprintf '%s %s' \"$SHOAL_HOOK\" \"$SHOAL_REVIEW_MARKS\" > '{}'\nexit 3\n",
            record.display()
        ),
    );
    manager
        .write_local_config(
            &repo.id,
            Some(format!("hooks.post_ready = '{}'\n", hook.display())),
        )
        .await
        .unwrap();
    let marks = manager
        .spaces
        .mark_ready(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Selection::default(),
            None,
        )
        .await
        .unwrap();
    let recorded = std::fs::read_to_string(&record).unwrap();
    let (name, json) = recorded.split_once(' ').unwrap();
    assert_eq!(name, "post_ready");
    assert_eq!(
        serde_json::from_str::<Vec<ReviewMark>>(json).unwrap(),
        marks.marks
    );
    assert_eq!(
        manager
            .spaces
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .review
            .len(),
        1
    );
    let notifications = manager.spaces.notifications.list(true, 10).await.unwrap();
    assert!(
        notifications
            .iter()
            .any(|notification| notification.kind == AlertKind::HookFailed)
    );
}
