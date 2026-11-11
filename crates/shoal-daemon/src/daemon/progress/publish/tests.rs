use std::sync::{Arc, Mutex};

use rusqlite::params;

use super::*;
use crate::{
    client::progress::{PlanStep, ProgressChange, StepState},
    daemon::test_support::{configure, git, manager, repository},
    forge::{ForgeRepo, items::link::Selection, test_support::Server},
};

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

/// A GitHub on a loopback port that serves PR 12 of acme/widgets, open and
/// out of draft at `head`, with the body its edits leave.
struct FakePull {
    server: Server,
    body: Arc<Mutex<String>>,
}

impl FakePull {
    async fn start(manager: &Manager, head: &str, body: &str) -> Self {
        let body = Arc::new(Mutex::new(body.to_owned()));
        let (kept, head) = (body.clone(), head.to_owned());
        let server = Server::start(move |request| {
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
                "draft": false, "node_id": "PR_1", "body": *kept.lock().unwrap(),
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
                "[cleanup]\ngrace_minutes = 0\n\
                 [machine.forges.\"{host}\"]\ntype = \"github\"\nagent_token = \"agent\"\n"
            ),
        )
        .await;
        Self { server, body }
    }

    fn remote(&self) -> String {
        format!("http://{}/acme/widgets.git", self.server.host)
    }

    fn body(&self) -> String {
        self.body.lock().unwrap().clone()
    }

    fn edits(&self) -> usize {
        (self.server.received().iter())
            .filter(|request| request.line.starts_with("PATCH"))
            .count()
    }
}

#[tokio::test]
async fn open_prs_show_the_progress_until_marked_ready() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "fix".into(), None, None, None)
        .await
        .unwrap();
    let head = crate::daemon::links::pr::current_head(&workspace)
        .await
        .unwrap();
    let pull = FakePull::start(&manager, &head, "Closes #3\n").await;
    git(
        &workspace.path,
        &["remote", "add", "origin", &pull.remote()],
    );
    let (_, url) = ForgeRepo::parse(&pull.remote())
        .unwrap()
        .pull("12")
        .unwrap();
    let (id, registration) = (workspace.id.clone(), serde_json::json!({ "url": url }));
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
    let selector = SpaceSelector::id(&workspace.id);
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

    manager.publish_progress(&workspace.id).await.unwrap();
    let section = "<!-- shoal:progress -->\n- [x] Reproduce\n<!-- /shoal:progress -->";
    assert_eq!(pull.body(), format!("Closes #3\n\n\n{section}"));
    assert!(manager.unpublished_progress().await.unwrap().is_empty());
    // A change after the edit waits to be published, whenever it came.
    let status = ProgressChange::Status(Some("Reproduced".into()));
    manager.set_progress(&selector, status).await.unwrap();
    let unpublished = manager.unpublished_progress().await.unwrap();
    assert_eq!(unpublished, std::slice::from_ref(&workspace.id));
    let cleared = ProgressChange::Status(None);
    manager.set_progress(&selector, cleared).await.unwrap();
    // A PR that already shows the progress is not edited again.
    manager.publish_progress(&workspace.id).await.unwrap();
    assert_eq!(pull.edits(), 1);

    // `[pr] progress = false` leaves the PR alone.
    let status = ProgressChange::Status(Some("Fixed".into()));
    manager.set_progress(&selector, status).await.unwrap();
    manager
        .write_local_config(&repo.id, Some("[pr]\nprogress = false\n".into()))
        .await
        .unwrap();
    manager.publish_progress(&workspace.id).await.unwrap();
    assert_eq!(pull.edits(), 1);
    manager.write_local_config(&repo.id, None).await.unwrap();

    // Ready removes the section, and later progress leaves the PR alone.
    manager
        .mark_ready(&selector, Selection::default(), false)
        .await
        .unwrap();
    assert_eq!(pull.body(), "Closes #3\n");
    manager.publish_progress(&workspace.id).await.unwrap();
    assert_eq!(pull.body(), "Closes #3\n");
}

#[tokio::test]
async fn a_pr_that_cannot_be_edited_leaves_the_others_updated_and_the_change_pending() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "fix".into(), None, None, None)
        .await
        .unwrap();
    let pull = FakePull::start(&manager, "abc", "").await;
    git(
        &workspace.path,
        &["remote", "add", "origin", &pull.remote()],
    );
    let forge = ForgeRepo::parse(&pull.remote()).unwrap();
    // PR 13, which the forge does not serve, comes first.
    let urls: Vec<String> = ["13", "12"]
        .iter()
        .map(|number| forge.pull(number).unwrap().1)
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
