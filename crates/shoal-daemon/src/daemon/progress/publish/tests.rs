use crate::forge::items::{link::LinkKind, reference::ItemRef};
use std::sync::{Arc, Mutex};

use rusqlite::params;

use super::*;
use crate::{
    daemon::test_support::{configure, git, manager, repository},
    forge::{ForgeRepo, items::link::Selection},
};
use shoal_forge::test_support::Server;
use shoal_protocol::progress::{PlanStep, ProgressChange, StepState};

#[test]
fn a_space_publishes_once_an_interval_and_then_its_latest_progress() {
    let start = Instant::now();
    let second = |seconds| start + Duration::from_secs(seconds);
    let mut schedule = Schedule::default();
    schedule.request("a".into(), start);
    assert_eq!(schedule.take_due(start), ["a"]);
    // A change within the interval waits for its end, another space does not.
    schedule.request("a".into(), second(10));
    schedule.request("b".into(), second(10));
    assert_eq!(schedule.take_due(second(10)), ["b"]);
    assert_eq!(schedule.next(), Some(start + INTERVAL));
    // Later changes join the one that waits.
    schedule.request("a".into(), second(30));
    assert!(schedule.take_due(second(59)).is_empty());
    assert_eq!(schedule.take_due(start + INTERVAL), ["a"]);
    assert_eq!(schedule.next(), None);
    // Past the interval, a change publishes at once.
    let later = start + INTERVAL * 3;
    schedule.request("a".into(), later);
    assert_eq!(schedule.take_due(later), ["a"]);
}

/// A GitHub on a loopback port that serves PR 12 of acme/widgets, open at
/// `head`, with the body its edits leave and the draft state its GraphQL
/// mutations set; the progress section is on.
struct FakePull {
    server: Server,
    body: Arc<Mutex<String>>,
    draft: Arc<Mutex<bool>>,
}

impl FakePull {
    async fn start(manager: &Manager, head: &str, body: &str, draft: bool) -> Self {
        let body = Arc::new(Mutex::new(body.to_owned()));
        let draft = Arc::new(Mutex::new(draft));
        let (kept, drafted, head) = (body.clone(), draft.clone(), head.to_owned());
        let server = Server::start(move |request| {
            if request.line.contains("graphql") {
                *drafted.lock().unwrap() = request.body.contains("convertPullRequestToDraft");
                return (200, Vec::new(), r#"{"data":{}}"#.into());
            }
            if !request.line.contains("pulls/12") {
                return (404, Vec::new(), r#"{"message":"not found"}"#.into());
            }
            if request.line.starts_with("PATCH") {
                let edit: serde_json::Value = serde_json::from_str(&request.body).unwrap();
                if let Some(edited) = edit["body"].as_str() {
                    *kept.lock().unwrap() = edited.to_owned();
                }
            }
            let pull = serde_json::json!({
                "number": 12, "title": "Fix", "state": "open", "merged": false,
                "draft": *drafted.lock().unwrap(), "node_id": "PR_1",
                "body": *kept.lock().unwrap(),
                "head": {"ref": "fix", "sha": head}, "base": {"ref": "main"},
                "labels": [],
            });
            (200, Vec::new(), pull.to_string().into())
        });
        let host = &server.host;
        crate::forge::hosts::learn(host, crate::forge::backends::ForgeKind::GitHub);
        configure(
            manager,
            &format!(
                "[cleanup]\ngrace_minutes = 0\n[pr]\nprogress = true\n\
                 [machine.forges.\"{host}\"]\ntype = \"github\"\nagent_token = \"agent\"\n"
            ),
        )
        .await;
        Self {
            server,
            body,
            draft,
        }
    }

    fn remote(&self) -> String {
        format!("http://{}/acme/widgets.git", self.server.host)
    }

    fn body(&self) -> String {
        self.body.lock().unwrap().clone()
    }

    fn set_draft(&self, draft: bool) {
        *self.draft.lock().unwrap() = draft;
    }

    fn edits(&self) -> usize {
        (self.server.received().iter())
            .filter(|request| request.line.starts_with("PATCH"))
            .count()
    }
}

/// A workspace whose assigned PR is PR 12 of a [`FakePull`] at its HEAD,
/// with `body` and `draft`.
struct Assigned {
    _temp: tempfile::TempDir,
    manager: Arc<Manager>,
    repo: crate::model::Repository,
    workspace: crate::model::Workspace,
    pull: FakePull,
}

impl Assigned {
    async fn start(body: &str, draft: bool) -> Self {
        let (temp, manager) = manager().await;
        let checkout = repository(temp.path(), "repo");
        let repo = manager
            .repositories
            .register(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "fix".into(), None, None, None)
            .await
            .unwrap();
        let head = shoal_links::pr::current_head(&workspace).await.unwrap();
        let pull = FakePull::start(&manager, head.as_str(), body, draft).await;
        git(
            &workspace.path,
            &["remote", "add", "origin", &pull.remote()],
        );
        let forge = ForgeRepo::parse(&pull.remote()).unwrap();
        let url = ItemRef::numbered(&forge, LinkKind::Pr, 12)
            .unwrap()
            .to_string();
        let (id, registration) = (workspace.id.clone(), serde_json::json!({ "urls": [url] }));
        manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
                    params![id, registration.to_string()],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        Self {
            _temp: temp,
            manager,
            repo,
            workspace,
            pull,
        }
    }

    fn selector(&self) -> SpaceSelector {
        SpaceSelector::id(&self.workspace.id)
    }

    async fn set_status(&self, status: &str) {
        let change = ProgressChange::Status(Some(status.into()));
        (self.manager.set_progress(&self.selector(), change))
            .await
            .unwrap();
    }

    async fn publish(&self) {
        (self.manager.publish_progress(&self.workspace.id))
            .await
            .unwrap();
    }

    /// Whether a publication of the workspace's progress was requested.
    fn requested(&self) -> bool {
        (self.manager.publications.take()).contains(&self.workspace.id)
    }
}

const SECTION: &str = "<!-- shoal:progress -->\n- [x] Reproduce\n<!-- /shoal:progress -->";

#[tokio::test]
async fn draft_prs_show_the_progress_until_they_leave_draft() {
    let assigned = Assigned::start("Closes #3\n", true).await;
    let (manager, pull, workspace) = (&assigned.manager, &assigned.pull, &assigned.workspace);
    let selector = assigned.selector();
    let steps = vec![PlanStep {
        name: "Reproduce".into(),
        state: StepState::Done,
        key: None,
    }];
    manager
        .set_progress(&selector, ProgressChange::Steps(steps))
        .await
        .unwrap();
    let unpublished = manager.unpublished_progress().await.unwrap();
    assert_eq!(unpublished, std::slice::from_ref(&workspace.id));

    assigned.publish().await;
    assert_eq!(pull.body(), format!("Closes #3\n\n\n{SECTION}"));
    assert!(manager.unpublished_progress().await.unwrap().is_empty());
    // A change after the edit waits to be published, whenever it came.
    assigned.set_status("Reproduced").await;
    let unpublished = manager.unpublished_progress().await.unwrap();
    assert_eq!(unpublished, std::slice::from_ref(&workspace.id));
    let cleared = ProgressChange::Status(None);
    manager.set_progress(&selector, cleared).await.unwrap();
    // A PR that already shows the progress is not edited again.
    assigned.publish().await;
    assert_eq!(pull.edits(), 1);

    // `[pr] progress = false` leaves the PR alone.
    assigned.set_status("Fixed").await;
    let off = Some("[pr]\nprogress = false\n".into());
    let repo = &assigned.repo.id;
    manager.write_local_config(repo, off).await.unwrap();
    assigned.publish().await;
    assert_eq!(pull.edits(), 1);
    manager.write_local_config(repo, None).await.unwrap();

    // Ready takes the PR out of draft, whose next publication removes the
    // section, and later progress leaves the PR alone.
    assigned.requested();
    manager
        .mark_ready(&selector, Selection::default(), None)
        .await
        .unwrap();
    assert!(assigned.requested());
    assigned.publish().await;
    assert_eq!(pull.body(), "Closes #3\n");
    assigned.set_status("Waiting for review").await;
    assigned.publish().await;
    assert_eq!(pull.body(), "Closes #3\n");

    // Returning to draft shows it again.
    manager
        .clear_ready(&selector, Selection::default(), Some(true))
        .await
        .unwrap();
    assert!(assigned.requested());
    assigned.publish().await;
    assert!(
        pull.body().ends_with("<!-- /shoal:progress -->"),
        "{}",
        pull.body()
    );
}

#[tokio::test]
async fn a_pr_out_of_draft_never_shows_the_progress() {
    let assigned = Assigned::start("Closes #3\n", false).await;
    assigned.set_status("Reproduced").await;
    assigned.publish().await;
    assert_eq!(assigned.pull.body(), "Closes #3\n");
    assert_eq!(assigned.pull.edits(), 0);
}

#[tokio::test]
async fn a_pr_the_forge_takes_out_of_draft_loses_the_progress() {
    let assigned = Assigned::start("Closes #3\n", true).await;
    let pull = &assigned.pull;
    assigned.set_status("Reproduced").await;
    assigned.publish().await;
    assert!(
        pull.body().contains("**Status:** Reproduced"),
        "{}",
        pull.body()
    );
    // The background poll's first read is its baseline, and a PR still in
    // draft asks for nothing.
    assigned
        .manager
        .links
        .poll_linked_item_activity(&*assigned.manager)
        .await
        .unwrap();
    assigned.requested();
    assigned
        .manager
        .links
        .poll_linked_item_activity(&*assigned.manager)
        .await
        .unwrap();
    assert!(!assigned.requested());

    pull.set_draft(false);
    assigned
        .manager
        .links
        .poll_linked_item_activity(&*assigned.manager)
        .await
        .unwrap();
    assert!(assigned.requested());
    assigned.publish().await;
    assert_eq!(pull.body(), "Closes #3\n");
}

#[tokio::test]
async fn a_pr_that_cannot_be_edited_leaves_the_others_updated_and_the_change_pending() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "fix".into(), None, None, None)
        .await
        .unwrap();
    let pull = FakePull::start(&manager, "abc", "", true).await;
    git(
        &workspace.path,
        &["remote", "add", "origin", &pull.remote()],
    );
    let forge = ForgeRepo::parse(&pull.remote()).unwrap();
    // PR 13, which the forge does not serve, comes first.
    let urls: Vec<String> = [13, 12]
        .into_iter()
        .map(|number| ItemRef::numbered(&forge, LinkKind::Pr, number).unwrap())
        .map(|url| url.to_string())
        .collect();
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            for (at, url) in urls.iter().enumerate() {
                db.execute(
                    "INSERT INTO space_items(space_id,relation,kind,url,created_at)
                     VALUES (?1,'opened','pr',?2,?3)",
                    params![id, url, i64::try_from(at)?],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let status = ProgressChange::Status(Some("Reproduced".into()));
    let selector = SpaceSelector::id(&workspace.id);
    manager.set_progress(&selector, status).await.unwrap();

    manager.publish_progress(&workspace.id).await.unwrap();
    assert!(
        pull.body().contains("**Status:** Reproduced"),
        "{}",
        pull.body()
    );
    let unpublished = manager.unpublished_progress().await.unwrap();
    assert_eq!(unpublished, std::slice::from_ref(&workspace.id));
}
