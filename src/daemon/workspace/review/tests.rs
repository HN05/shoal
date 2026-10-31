use super::*;
use crate::{
    daemon::{
        events::{EventItem, EventKind},
        scope::{self, Caller},
    },
    forge::ForgeRepo,
    protocol::AssignmentMethod,
    test_support::{commit, git, install_test_script, manager, repository},
};

const REMOTE: &str = "https://github.com/acme/widgets.git";

fn select(kind: Option<LinkKind>, input: Option<&str>) -> Selection {
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
            EventItem::Chat(_) | EventItem::Gap { .. } => None,
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
    // Draft state and convention warnings are covered below; this forge
    // only keeps marking away from a real one.
    let forge_dir = temp.path().join("forge");
    std::fs::create_dir(&forge_dir).unwrap();
    let gh = fake_gh(&forge_dir);
    let config = format!(
        "[pr]\nundraft_on_ready = false\n[agent.forge_clis]\ngh = '{}'\n",
        gh.display()
    );
    manager
        .write_local_config(&repo.id, Some(config))
        .await
        .unwrap();
    git(&workspace.path, &["remote", "add", "origin", REMOTE]);
    let forge = ForgeRepo::parse(REMOTE).unwrap();
    let (_, pr) = forge.pull("12").unwrap();
    let (_, issue) = forge.issue("7").unwrap();
    let head = current_head(&workspace).await.unwrap();

    // Nothing linked: the workspace itself is marked, but a kind needs links.
    let marks = manager
        .mark_ready(&workspace.id, Selection::default(), false)
        .await
        .unwrap();
    assert_eq!((marks[0].kind, marks[0].url.as_deref()), (None, None));
    assert!(
        manager
            .mark_ready(&workspace.id, select(Some(LinkKind::Pr), None), false)
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
    let listed = manager.list_workspaces().await.unwrap();
    assert_eq!(
        listed[0].links,
        crate::model::WorkspaceLinks {
            issue: Some(issue.clone()),
            issue_title: None,
            prs: vec![pr.clone()],
            ..Default::default()
        }
    );
    let inspection = manager.inspect_workspace(&workspace.id).await.unwrap();
    assert_eq!(inspection.workspace.links, listed[0].links);
    let marks = manager
        .mark_ready(&workspace.id, Selection::default(), false)
        .await
        .unwrap();
    let mut marked: Vec<_> = marks.iter().map(|mark| mark.url.clone().unwrap()).collect();
    marked.sort();
    let mut expected = vec![issue.clone(), pr.clone()];
    expected.sort();
    assert_eq!(marked, expected);
    // Linked-item marks replace the mark made while nothing was linked.
    assert_eq!(
        manager.workspace(&workspace.id).await.unwrap().review.len(),
        2
    );
    assert!(
        manager
            .mark_ready(&workspace.id, select(Some(LinkKind::Pr), Some("13")), false)
            .await
            .is_err(),
        "an unlinked PR cannot be marked"
    );

    // A new commit makes existing marks stale until the agent marks again.
    std::fs::write(workspace.path.join("later"), "later\n").unwrap();
    commit(&workspace.path, "later");
    let inspection = manager.inspect_workspace(&workspace.id).await.unwrap();
    assert_eq!(inspection.workspace.review.len(), 2);
    assert!(
        inspection
            .workspace
            .review
            .iter()
            .all(|mark| mark.stale == Some(true))
    );
    let later = current_head(&workspace).await.unwrap();
    manager
        .mark_ready(&workspace.id, select(Some(LinkKind::Pr), Some("12")), false)
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
        .clear_ready(&workspace.id, select(Some(LinkKind::Pr), Some("12")), None)
        .await
        .unwrap();
    assert_eq!(cleared.len(), 1);
    assert!(
        manager
            .clear_ready(&workspace.id, select(Some(LinkKind::Pr), Some("12")), None)
            .await
            .is_err()
    );
    // So does withdrawing every mark, or every mark of a kind.
    for selection in [Selection::default(), select(Some(LinkKind::Pr), None)] {
        let error = manager
            .clear_ready(&workspace.id, selection, None)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("marked ready"), "{error:#}");
    }
    assert!(
        manager
            .workspace(&workspace.id)
            .await
            .unwrap()
            .review
            .is_empty()
    );

    let ready = EventKind::ReviewReady.to_string();
    let cleared = EventKind::ReviewCleared.to_string();
    let mut events = review_events(&manager).await;
    // Initial marks of the two linked items share one transaction; order them.
    events[2..4].sort();
    let mut linked = vec![
        (ready.clone(), Some(issue.clone()), head.clone()),
        (ready.clone(), Some(pr.clone()), head.clone()),
    ];
    linked.sort();
    assert_eq!(
        events,
        [
            vec![
                (ready.clone(), None, head.clone()),
                (cleared.clone(), None, head.clone()),
            ],
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
    manager
        .mark_ready(&workspace.id, Selection::default(), false)
        .await
        .unwrap();
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("DELETE FROM workspaces WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(review_events(&manager).await.len(), 8);
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
        .executions
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
            AssignmentMethod::MarkReady {
                workspace: target.clone(),
                selection: Selection::default(),
            },
            AssignmentMethod::ClearReady {
                workspace: target.clone(),
                selection: Selection::default(),
                draft: None,
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
    install_test_script(
        &hook,
        &format!(
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
        .mark_ready(&workspace.id, Selection::default(), false)
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
                .contains("hooks.post_ready exited with 3")
    }));
}

/// A `gh` that serves PR 12 of [`REMOTE`] with the draft state in `state`
/// and no labels, and the repository's one label; changes draft state
/// through GraphQL, and fails while `fail` exists.
fn fake_gh(dir: &std::path::Path) -> std::path::PathBuf {
    let gh = dir.join("gh-agent");
    let state = dir.join("state");
    let fail = dir.join("fail");
    let log = dir.join("log");
    install_test_script(
        &gh,
        &format!(
            r#"#!/bin/sh
input=$(cat)
echo "$*" >> '{log}'
[ -e '{fail}' ] && {{ printf 'HTTP/2.0 403 Forbidden\n\n{{"message":"forbidden"}}\n'; exit 1; }}
printf 'HTTP/2.0 200 OK\n\n'
case "$*" in
  *graphql*)
    case "$input" in
      *markPullRequestReadyForReview*) echo false > '{state}' ;;
      *convertPullRequestToDraft*) echo true > '{state}' ;;
    esac
    echo '{{"data":{{}}}}' ;;
  */labels*)
    echo '[{{"name":"type/bug","description":"Broken"}}]' ;;
  *pulls/12*)
    printf '{{"number":12,"title":"Fix","state":"open","draft":%s,"node_id":"PR_1","head":{{"ref":"ready"}},"base":{{"ref":"main"}},"labels":[]}}' "$(cat '{state}')" ;;
esac
"#,
            log = log.display(),
            fail = fail.display(),
            state = state.display(),
        ),
    );
    std::fs::write(&state, "true\n").unwrap();
    gh
}

#[tokio::test]
async fn ready_takes_marked_prs_out_of_draft_and_unready_can_return_them() {
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
    let forge_dir = temp.path().join("forge");
    std::fs::create_dir(&forge_dir).unwrap();
    let gh = fake_gh(&forge_dir);
    let config = |extra: &str| format!("[agent.forge_clis]\ngh = '{}'\n{extra}", gh.display());
    manager
        .write_local_config(&repo.id, Some(config("")))
        .await
        .unwrap();
    let (_, pr) = ForgeRepo::parse(REMOTE).unwrap().pull("12").unwrap();
    let id = workspace.id.clone();
    let registration = serde_json::json!({ "url": pr });
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO pr_cleanup(workspace_id,record) VALUES (?1,?2)",
                params![id, registration.to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let draft = || std::fs::read_to_string(forge_dir.join("state")).unwrap() == "true\n";
    let mutations = || {
        std::fs::read_to_string(forge_dir.join("log"))
            .unwrap_or_default()
            .matches("graphql")
            .count()
    };

    // Marking undrafts by default, and a PR already out of draft is left alone.
    // The unlabeled PR comes back with a warning that is never stored.
    let marks = manager
        .mark_ready(&workspace.id, Selection::default(), false)
        .await
        .unwrap();
    assert!(!draft());
    assert!(
        marks[0].warnings[0].starts_with("PR #12 has no labels"),
        "{:?}",
        marks[0].warnings
    );
    let stored = manager.workspace(&workspace.id).await.unwrap().review;
    assert!(stored[0].warnings.is_empty());
    manager
        .mark_ready(&workspace.id, Selection::default(), false)
        .await
        .unwrap();
    assert_eq!(mutations(), 1);
    let changes = manager
        .workspace_events(None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|item| {
            matches!(item, EventItem::Event(event) if event.details.kind == EventKind::ItemChanged)
        })
        .count();
    assert_eq!(changes, 1);

    // Withdrawing leaves the PR ready unless asked to return it to draft.
    manager
        .clear_ready(&workspace.id, Selection::default(), None)
        .await
        .unwrap();
    assert!(!draft());
    manager
        .mark_ready(&workspace.id, Selection::default(), false)
        .await
        .unwrap();
    manager
        .clear_ready(&workspace.id, Selection::default(), Some(true))
        .await
        .unwrap();
    assert!(draft());

    // `[unready] draft` returns it without the flag; `[ready] undraft = false`
    // keeps a mark local.
    manager
        .write_local_config(
            &repo.id,
            Some(config(
                "[pr]\nundraft_on_ready = false\ndraft_on_unready = true\n",
            )),
        )
        .await
        .unwrap();
    manager
        .mark_ready(&workspace.id, Selection::default(), false)
        .await
        .unwrap();
    assert!(draft());
    manager
        .write_local_config(&repo.id, Some(config("[pr]\ndraft_on_unready = true\n")))
        .await
        .unwrap();
    manager
        .mark_ready(&workspace.id, Selection::default(), false)
        .await
        .unwrap();
    assert!(!draft());
    manager
        .clear_ready(&workspace.id, Selection::default(), None)
        .await
        .unwrap();
    assert!(draft());

    // A forge failure keeps the mark either way, so the command can be retried.
    std::fs::write(forge_dir.join("fail"), "").unwrap();
    let error = manager
        .mark_ready(&workspace.id, Selection::default(), false)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("still a draft"), "{error:#}");
    let marks = || async { manager.workspace(&workspace.id).await.unwrap().review.len() };
    assert_eq!(marks().await, 1);
    std::fs::remove_file(forge_dir.join("fail")).unwrap();
    manager
        .mark_ready(&workspace.id, Selection::default(), false)
        .await
        .unwrap();
    assert!(!draft());
    std::fs::write(forge_dir.join("fail"), "").unwrap();
    let pr_only = || select(Some(LinkKind::Pr), Some("12"));
    let error = manager
        .clear_ready(&workspace.id, pr_only(), Some(true))
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("kept the marks"), "{error:#}");
    assert_eq!(marks().await, 1);
    std::fs::remove_file(forge_dir.join("fail")).unwrap();
    manager
        .clear_ready(&workspace.id, pr_only(), Some(true))
        .await
        .unwrap();
    assert!(draft());
    assert_eq!(marks().await, 0);
}
