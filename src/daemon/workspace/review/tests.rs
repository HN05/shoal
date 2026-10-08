use super::*;
use crate::{
    daemon::{
        events::{EventItem, EventKind},
        scope::{self, Caller},
    },
    forge::ForgeRepo,
    protocol::Method,
    test_support::{commit, git, manager, repository},
};

const REMOTE: &str = "https://github.com/acme/widgets.git";

fn select(kind: Option<ItemKind>, input: Option<&str>) -> Selection {
    Selection {
        kind,
        input: input.map(str::to_owned),
    }
}

async fn review_events(manager: &Manager) -> Vec<(String, Option<String>, String)> {
    manager
        .workspace_events(None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            EventItem::Event(event) => event
                .details
                .review
                .map(|review| (event.details.kind.to_string(), review.url, review.head)),
            EventItem::Gap { .. } => None,
        })
        .collect()
}

#[tokio::test]
async fn marks_bind_to_head_follow_links_and_record_events() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "ready".into(), None, None, None)
        .await
        .unwrap();
    git(&workspace.path, &["remote", "add", "origin", REMOTE]);
    let forge = ForgeRepo::parse(REMOTE).unwrap();
    let (_, pr) = forge.pull("12").unwrap();
    let (_, issue) = forge.issue("7").unwrap();
    let head = current_head(&workspace).await.unwrap();

    // Nothing linked: the workspace itself is marked, but a kind needs links.
    let marks = manager
        .mark_ready(&workspace.id, Selection::default())
        .await
        .unwrap();
    assert_eq!((marks[0].kind, marks[0].url.as_deref()), (None, None));
    assert!(
        manager
            .mark_ready(&workspace.id, select(Some(ItemKind::Pr), None))
            .await
            .is_err()
    );

    let id = workspace.id.clone();
    let registration = serde_json::json!({ "url": pr });
    let linked_issue = issue.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO pr_cleanup(workspace_id,record) VALUES (?1,?2)",
                params![id, registration.to_string()],
            )?;
            db.execute(
                "INSERT INTO workspace_issue(workspace_id,url) VALUES (?1,?2)",
                params![id, linked_issue],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let marks = manager
        .mark_ready(&workspace.id, Selection::default())
        .await
        .unwrap();
    let mut marked: Vec<_> = marks.iter().map(|mark| mark.url.clone().unwrap()).collect();
    marked.sort();
    let mut expected = vec![issue.clone(), pr.clone()];
    expected.sort();
    assert_eq!(marked, expected);
    assert!(
        manager
            .mark_ready(&workspace.id, select(Some(ItemKind::Pr), Some("13")))
            .await
            .is_err(),
        "an unlinked PR cannot be marked"
    );

    // A new commit makes existing marks stale until the agent marks again.
    std::fs::write(workspace.path.join("later"), "later\n").unwrap();
    commit(&workspace.path, "later");
    let inspection = manager.inspect_workspace(&workspace.id).await.unwrap();
    assert_eq!(inspection.workspace.review.len(), 3);
    assert!(
        inspection
            .workspace
            .review
            .iter()
            .all(|mark| mark.stale == Some(true))
    );
    let later = current_head(&workspace).await.unwrap();
    manager
        .mark_ready(&workspace.id, select(Some(ItemKind::Pr), Some("12")))
        .await
        .unwrap();
    let inspection = manager.inspect_workspace(&workspace.id).await.unwrap();
    let pr_mark = inspection
        .workspace
        .review
        .iter()
        .find(|mark| mark.url.as_ref() == Some(&pr))
        .unwrap();
    assert_eq!(
        (pr_mark.head.as_str(), pr_mark.stale),
        (later.as_str(), Some(false))
    );

    // Unlinking withdraws a mark; explicit clearing needs an existing mark.
    manager.clear_issue(&workspace.id, None).await.unwrap();
    let cleared = manager
        .clear_ready(&workspace.id, select(Some(ItemKind::Pr), Some("12")))
        .await
        .unwrap();
    assert_eq!(cleared.len(), 1);
    assert!(
        manager
            .clear_ready(&workspace.id, select(Some(ItemKind::Pr), Some("12")))
            .await
            .is_err()
    );
    let remaining = manager.workspace(&workspace.id).await.unwrap().review;
    assert_eq!((remaining.len(), remaining[0].url.as_ref()), (1, None));

    let ready = EventKind::ReviewReady.to_string();
    let cleared = EventKind::ReviewCleared.to_string();
    let mut events = review_events(&manager).await;
    // Initial marks of the two linked items share one transaction; order them.
    events[1..3].sort();
    let mut linked = vec![
        (ready.clone(), Some(issue.clone()), head.clone()),
        (ready.clone(), Some(pr.clone()), head.clone()),
    ];
    linked.sort();
    assert_eq!(
        events,
        [
            vec![(ready.clone(), None, head.clone())],
            linked,
            vec![
                (ready.clone(), Some(pr.clone()), later.clone()),
                (cleared.clone(), Some(issue), head.clone()),
                (cleared.clone(), Some(pr), later),
            ],
        ]
        .concat()
    );

    // Removal cascades marks away without reporting each as withdrawn.
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("DELETE FROM workspaces WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(review_events(&manager).await.len(), 6);
}

#[tokio::test]
async fn scoped_callers_mark_only_their_own_workspace() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let own = manager
        .create_workspace(&repo.id, "own".into(), None, None, None)
        .await
        .unwrap();
    let other = manager
        .create_workspace(&repo.id, "other".into(), None, None, None)
        .await
        .unwrap();
    manager
        .issue_scope(
            "scope".into(),
            Caller {
                workspace_id: own.id.clone(),
                execution: None,
            },
        )
        .await;
    for (target, allowed) in [(&own.name, true), (&other.name, false)] {
        for mut method in [
            Method::MarkReady {
                workspace: target.clone(),
                selection: Selection::default(),
            },
            Method::ClearReady {
                workspace: target.clone(),
                selection: Selection::default(),
            },
        ] {
            assert_eq!(
                scope::authorize(&manager, Some("scope"), &mut method)
                    .await
                    .is_ok(),
                allowed
            );
        }
    }
}

#[tokio::test]
async fn post_ready_hook_receives_marks_and_its_failure_keeps_them() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, manager) = manager().await;
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
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\nprintf '%s %s' \"$SHOAL_HOOK\" \"$SHOAL_REVIEW_MARKS\" > '{}'\nexit 3\n",
            record.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    manager
        .set_repository_config(
            &repo.id,
            Some(format!("post_ready_cmd = '{}'\n", hook.display())),
        )
        .await
        .unwrap();
    let marks = manager
        .mark_ready(&workspace.id, Selection::default())
        .await
        .unwrap();
    let recorded = std::fs::read_to_string(&record).unwrap();
    let (name, json) = recorded.split_once(' ').unwrap();
    assert_eq!(name, "post_ready");
    assert_eq!(
        serde_json::from_str::<Vec<ReviewMark>>(json).unwrap(),
        marks
    );
    assert_eq!(
        manager.workspace(&workspace.id).await.unwrap().review.len(),
        1
    );
    let notifications = manager.notifications(true, 10).await.unwrap();
    assert!(notifications.iter().any(|notification| {
        notification.kind == NotificationKind::HookFailed
            && notification
                .message
                .contains("post_ready_cmd exited with 3")
    }));
}
