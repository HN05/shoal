use super::*;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        events::{EventItem, EventKind},
        scope::{self, Caller},
        test_support::{commit, configure, git, install_test_script, manager, repository},
    },
    forge::{ForgeRepo, test_support::Server},
    protocol::AssignmentMethod,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

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
            EventItem::Chat(_)
            | EventItem::File(_)
            | EventItem::Shoal(_)
            | EventItem::Gap { .. } => None,
        })
        .collect()
}

/// Link the `pr`, and the `issue` if given, to the space `id`.
async fn link(manager: &Manager, id: &str, pr: &str, issue: Option<&str>) {
    let id = id.to_owned();
    let registration = serde_json::json!({ "url": pr });
    let issue = issue.map(str::to_owned);
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
                params![id, registration.to_string()],
            )?;
            if let Some(issue) = issue {
                db.execute(
                    "INSERT INTO space_issue(space_id,url) VALUES (?1,?2)",
                    params![id, issue],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
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
    let github = FakeGitHub::start(&manager).await;
    let config = "[pr]\nundraft_on_ready = false\n".to_owned();
    manager
        .write_local_config(&repo.id, Some(config))
        .await
        .unwrap();
    git(
        &workspace.path,
        &["remote", "add", "origin", &github.remote()],
    );
    let forge = ForgeRepo::parse(&github.remote()).unwrap();
    let (_, pr) = forge.pull("12").unwrap();
    let (_, issue) = forge.issue("7").unwrap();
    let head = current_head(&workspace).await.unwrap();
    github.push(&head);

    // Nothing linked: the workspace itself is marked, but a kind needs links.
    let marks = manager
        .mark_ready(
            &SpaceSelector::id(&workspace.id),
            Selection::default(),
            false,
        )
        .await
        .unwrap()
        .marks;
    assert_eq!((marks[0].kind, marks[0].url.as_deref()), (None, None));
    assert!(
        manager
            .mark_ready(
                &SpaceSelector::id(&workspace.id),
                select(Some(LinkKind::Pr), None),
                false
            )
            .await
            .is_err()
    );

    link(&manager, workspace.id.as_str(), &pr, Some(&issue)).await;
    let listed = manager.list_workspaces().await.unwrap();
    assert_eq!(
        listed[0].links,
        crate::model::SpaceLinks {
            issue: Some(issue.clone()),
            issue_title: None,
            prs: vec![pr.clone()],
            ..Default::default()
        }
    );
    let inspection = manager
        .inspect_workspace(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(inspection.workspace.links, listed[0].links);
    let marks = manager
        .mark_ready(
            &SpaceSelector::id(&workspace.id),
            Selection::default(),
            false,
        )
        .await
        .unwrap()
        .marks;
    let mut marked: Vec<_> = marks.iter().map(|mark| mark.url.clone().unwrap()).collect();
    marked.sort();
    let mut expected = vec![issue.clone(), pr.clone()];
    expected.sort();
    assert_eq!(marked, expected);
    // Linked-item marks replace the mark made while nothing was linked.
    assert_eq!(
        manager
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .review
            .len(),
        2
    );
    assert!(
        manager
            .mark_ready(
                &SpaceSelector::id(&workspace.id),
                select(Some(LinkKind::Pr), Some("13")),
                false
            )
            .await
            .is_err(),
        "an unlinked PR cannot be marked"
    );

    // A new commit makes existing marks stale until the agent marks again.
    std::fs::write(workspace.path.join("later"), "later\n").unwrap();
    commit(&workspace.path, "later");
    let inspection = manager
        .inspect_workspace(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(inspection.workspace.review.len(), 2);
    assert!(
        inspection
            .workspace
            .review
            .iter()
            .all(|mark| mark.stale == Some(true))
    );
    let later = current_head(&workspace).await.unwrap();
    let pr_only = || select(Some(LinkKind::Pr), Some("12"));
    // The PR is marked only once its head on the forge is HEAD.
    assert!(
        manager
            .mark_ready(&SpaceSelector::id(&workspace.id), pr_only(), false)
            .await
            .is_err()
    );
    github.push(&later);
    manager
        .mark_ready(&SpaceSelector::id(&workspace.id), pr_only(), false)
        .await
        .unwrap();
    let inspection = manager
        .inspect_workspace(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
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
    manager
        .clear_issue(&SpaceSelector::id(&workspace.id), None)
        .await
        .unwrap();
    let cleared = manager
        .clear_ready(
            &SpaceSelector::id(&workspace.id),
            select(Some(LinkKind::Pr), Some("12")),
            None,
        )
        .await
        .unwrap();
    assert_eq!(cleared.len(), 1);
    assert!(
        manager
            .clear_ready(
                &SpaceSelector::id(&workspace.id),
                select(Some(LinkKind::Pr), Some("12")),
                None
            )
            .await
            .is_err()
    );
    // So does withdrawing every mark, or every mark of a kind.
    for selection in [Selection::default(), select(Some(LinkKind::Pr), None)] {
        let cleared = manager
            .clear_ready(&SpaceSelector::id(&workspace.id), selection, None)
            .await;
        assert!(cleared.is_err());
    }
    assert!(
        manager
            .workspace_by_id(&workspace.id)
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
        .mark_ready(
            &SpaceSelector::id(&workspace.id),
            Selection::default(),
            false,
        )
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
async fn ready_skips_linked_prs_that_are_closed_or_at_another_head() {
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
    let github = FakeGitHub::start(&manager).await;
    git(
        &workspace.path,
        &["remote", "add", "origin", &github.remote()],
    );
    let forge = ForgeRepo::parse(&github.remote()).unwrap();
    let (_, pr) = forge.pull("12").unwrap();
    let (_, issue) = forge.issue("7").unwrap();
    link(&manager, workspace.id.as_str(), &pr, Some(&issue)).await;
    github.push(&current_head(&workspace).await.unwrap());
    let marked_urls = |ready: &ReadyMarks| -> Vec<String> {
        (ready.marks.iter())
            .filter_map(|mark| mark.url.clone())
            .collect()
    };

    // An unpushed commit leaves the PR unmarked and says why.
    std::fs::write(workspace.path.join("later"), "later\n").unwrap();
    commit(&workspace.path, "later");
    let ready = manager
        .mark_ready(
            &SpaceSelector::id(&workspace.id),
            Selection::default(),
            false,
        )
        .await
        .unwrap();
    assert_eq!(marked_urls(&ready), std::slice::from_ref(&issue));
    assert_eq!(ready.skipped.len(), 1);
    assert_eq!(ready.skipped[0].url, pr);

    // A merged PR stays unmarked whatever its head, and selecting it fails.
    github.push(&current_head(&workspace).await.unwrap());
    github.merge();
    let ready = manager
        .mark_ready(
            &SpaceSelector::id(&workspace.id),
            Selection::default(),
            false,
        )
        .await
        .unwrap();
    assert_eq!(marked_urls(&ready), [issue]);
    let skipped: Vec<_> = ready.skipped.iter().map(|skip| &skip.url).collect();
    assert_eq!(skipped, [&pr]);
    for selection in [
        select(Some(LinkKind::Pr), Some("12")),
        select(Some(LinkKind::Pr), None),
    ] {
        let marked = manager
            .mark_ready(&SpaceSelector::id(&workspace.id), selection, false)
            .await;
        assert!(marked.is_err());
    }
    let stored = manager.workspace_by_id(&workspace.id).await.unwrap().review;
    assert!(stored.iter().all(|mark| mark.url.as_ref() != Some(&pr)));
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
                space_id: own.id.clone(),
                execution: None,
            },
        )
        .await;
    for (target, allowed) in [(&own.name, true), (&other.name, false)] {
        for mut method in [
            AssignmentMethod::MarkReady {
                space: SpaceSelector::parse(target),
                selection: Selection::default(),
            },
            AssignmentMethod::ClearReady {
                space: SpaceSelector::parse(target),
                selection: Selection::default(),
                draft: None,
            },
        ] {
            assert_eq!(
                scope::authorize(&manager, Some(&"scope".into()), &mut method)
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
        .mark_ready(
            &SpaceSelector::id(&workspace.id),
            Selection::default(),
            false,
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
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .review
            .len(),
        1
    );
    let notifications = manager.notifications(true, 10).await.unwrap();
    assert!(
        notifications
            .iter()
            .any(|notification| notification.kind == NotificationKind::HookFailed)
    );
}

#[tokio::test]
async fn a_chats_post_ready_hook_gets_the_configured_environment() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "planning".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    let record = temp.path().join("environment");
    let hook = temp.path().join("post-ready");
    install_test_script(
        &hook,
        &format!(
            "#!/bin/sh\nprintf '%s|%s' \"$GREETING\" \"$SHOAL_CHAT\" > '{}'\n",
            record.display()
        ),
    );
    manager
        .write_local_config(
            &repo.id,
            Some(format!(
                "hooks.post_ready = '{}'\n[env]\nGREETING = 'hello from {{repo}} in {{path}}'\n",
                hook.display()
            )),
        )
        .await
        .unwrap();
    manager
        .mark_ready(&SpaceSelector::id(&chat.id), Selection::default(), false)
        .await
        .unwrap();
    let recorded = std::fs::read_to_string(&record).unwrap();
    assert_eq!(
        recorded,
        format!("hello from repo in {}|{}", repo.path.display(), chat.id)
    );
}

/// A GitHub on a loopback port that serves PR 12 of acme/widgets with its
/// head, merge and draft state and no labels, and the repository's one label;
/// it changes draft state through GraphQL, which it refuses while it fails.
struct FakeGitHub {
    server: Server,
    head: Arc<Mutex<String>>,
    merged: Arc<AtomicBool>,
    draft: Arc<AtomicBool>,
    fail: Arc<AtomicBool>,
}

impl FakeGitHub {
    /// Serve a draft PR, and give the agent a token for the host.
    async fn start(manager: &Manager) -> Self {
        let head = Arc::new(Mutex::new(String::new()));
        let merged = Arc::new(AtomicBool::new(false));
        let draft = Arc::new(AtomicBool::new(true));
        let fail = Arc::new(AtomicBool::new(false));
        let (sha, merge) = (head.clone(), merged.clone());
        let (state, failing) = (draft.clone(), fail.clone());
        let server = Server::start(move |request| {
            let body = if request.line.ends_with("/graphql") {
                if failing.load(Ordering::SeqCst) {
                    return (403, Vec::new(), r#"{"message":"forbidden"}"#.into());
                }
                if request.body.contains("markPullRequestReadyForReview") {
                    state.store(false, Ordering::SeqCst);
                } else if request.body.contains("convertPullRequestToDraft") {
                    state.store(true, Ordering::SeqCst);
                }
                r#"{"data":{}}"#.to_owned()
            } else if request.line.contains("/labels") {
                r#"[{"name":"type/bug","description":"Broken"}]"#.to_owned()
            } else if request.line.contains("pulls/12") {
                let merged = merge.load(Ordering::SeqCst);
                format!(
                    r#"{{"number":12,"title":"Fix","state":"{}","merged":{merged},"draft":{},"node_id":"PR_1","head":{{"ref":"ready","sha":"{}"}},"base":{{"ref":"main"}},"labels":[]}}"#,
                    if merged { "closed" } else { "open" },
                    state.load(Ordering::SeqCst),
                    sha.lock().unwrap(),
                )
            } else {
                return (404, Vec::new(), r#"{"message":"not found"}"#.into());
            };
            (200, Vec::new(), body.into())
        });
        let github = Self {
            server,
            head,
            merged,
            draft,
            fail,
        };
        let host = &github.server.host;
        // Pins are this process's, which parallel tests replace; an answer stays.
        crate::forge::hosts::learn(host, crate::forge::backends::ForgeKind::GitHub);
        configure(
            manager,
            &format!(
                "[cleanup]\ngrace_minutes = 0\n\
                 [machine.forges.\"{host}\"]\ntype = \"github\"\nagent_token = \"agent\"\n"
            ),
        )
        .await;
        github
    }

    fn remote(&self) -> String {
        format!("http://{}/acme/widgets.git", self.server.host)
    }

    /// Report `head` as the PR's head commit.
    fn push(&self, head: &str) {
        *self.head.lock().unwrap() = head.to_owned();
    }

    fn merge(&self) {
        self.merged.store(true, Ordering::SeqCst);
    }

    fn draft(&self) -> bool {
        self.draft.load(Ordering::SeqCst)
    }

    fn fail(&self, failing: bool) {
        self.fail.store(failing, Ordering::SeqCst);
    }

    /// How many draft changes it received.
    fn mutations(&self) -> usize {
        self.server
            .received()
            .iter()
            .filter(|request| request.line.ends_with("/graphql"))
            .count()
    }
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
    let github = FakeGitHub::start(&manager).await;
    git(
        &workspace.path,
        &["remote", "add", "origin", &github.remote()],
    );
    let (_, pr) = ForgeRepo::parse(&github.remote())
        .unwrap()
        .pull("12")
        .unwrap();
    link(&manager, workspace.id.as_str(), &pr, None).await;
    github.push(&current_head(&workspace).await.unwrap());
    let draft = || github.draft();
    let mutations = || github.mutations();

    // Marking undrafts by default, and a PR already out of draft is left alone.
    // The unlabeled PR comes back with a warning that is never stored.
    let marks = manager
        .mark_ready(
            &SpaceSelector::id(&workspace.id),
            Selection::default(),
            false,
        )
        .await
        .unwrap()
        .marks;
    assert!(!draft());
    assert!(!marks[0].warnings.is_empty());
    let stored = manager.workspace_by_id(&workspace.id).await.unwrap().review;
    assert!(stored[0].warnings.is_empty());
    manager
        .mark_ready(
            &SpaceSelector::id(&workspace.id),
            Selection::default(),
            false,
        )
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
        .clear_ready(
            &SpaceSelector::id(&workspace.id),
            Selection::default(),
            None,
        )
        .await
        .unwrap();
    assert!(!draft());
    manager
        .mark_ready(
            &SpaceSelector::id(&workspace.id),
            Selection::default(),
            false,
        )
        .await
        .unwrap();
    manager
        .clear_ready(
            &SpaceSelector::id(&workspace.id),
            Selection::default(),
            Some(true),
        )
        .await
        .unwrap();
    assert!(draft());

    // `[unready] draft` returns it without the flag; `[ready] undraft = false`
    // keeps a mark local.
    manager
        .write_local_config(
            &repo.id,
            Some(String::from(
                "[pr]\nundraft_on_ready = false\ndraft_on_unready = true\n",
            )),
        )
        .await
        .unwrap();
    manager
        .mark_ready(
            &SpaceSelector::id(&workspace.id),
            Selection::default(),
            false,
        )
        .await
        .unwrap();
    assert!(draft());
    manager
        .write_local_config(
            &repo.id,
            Some(String::from("[pr]\ndraft_on_unready = true\n")),
        )
        .await
        .unwrap();
    manager
        .mark_ready(
            &SpaceSelector::id(&workspace.id),
            Selection::default(),
            false,
        )
        .await
        .unwrap();
    assert!(!draft());
    manager
        .clear_ready(
            &SpaceSelector::id(&workspace.id),
            Selection::default(),
            None,
        )
        .await
        .unwrap();
    assert!(draft());

    // A forge failure keeps the mark either way, so the command can be retried.
    github.fail(true);
    assert!(
        manager
            .mark_ready(
                &SpaceSelector::id(&workspace.id),
                Selection::default(),
                false
            )
            .await
            .is_err()
    );
    let marks = || async {
        manager
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .review
            .len()
    };
    assert_eq!(marks().await, 1);
    github.fail(false);
    manager
        .mark_ready(
            &SpaceSelector::id(&workspace.id),
            Selection::default(),
            false,
        )
        .await
        .unwrap();
    assert!(!draft());
    github.fail(true);
    let pr_only = || select(Some(LinkKind::Pr), Some("12"));
    let cleared = manager
        .clear_ready(&SpaceSelector::id(&workspace.id), pr_only(), Some(true))
        .await;
    assert!(cleared.is_err());
    assert_eq!(marks().await, 1);
    github.fail(false);
    manager
        .clear_ready(&SpaceSelector::id(&workspace.id), pr_only(), Some(true))
        .await
        .unwrap();
    assert!(draft());
    assert_eq!(marks().await, 0);
}

#[tokio::test]
async fn listings_leave_staleness_unknown_where_file_access_stalls() {
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
    manager
        .mark_ready(
            &SpaceSelector::id(&workspace.id),
            Selection::default(),
            false,
        )
        .await
        .unwrap();
    let listed = manager.visible_workspaces(None).await.unwrap();
    assert_eq!(listed[0].review[0].stale, Some(false));

    tokio::time::pause();
    let stalled = manager
        .check_access(&workspace.path, std::future::pending::<Result<()>>())
        .await;
    tokio::time::resume();
    assert!(stalled.is_err());

    let listed = manager.visible_workspaces(None).await.unwrap();
    assert_eq!(listed[0].review[0].stale, None);
    let inspection = manager
        .inspect_workspace(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(inspection.workspace.review[0].stale, None);
}
