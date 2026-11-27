use crate::model::{QueueEntry, QueuedStart};
use crate::protocol::operations::{LaunchIssue, QueuedLaunch};
use crate::queue::tests::{claude, queued_in};
use crate::stand_in::Fixture;
use shoal_protocol::launches::LaunchSource;

/// A start that gives every field of its launch a queue request keeps,
/// with its caller starting it.
fn everything() -> QueuedStart {
    let launch = QueuedLaunch {
        prompt: Some("Fix the login".into()),
        mode: Some("plan".into()),
        narrowing: crate::config::permissions::PermissionChange::assigned("merge=direct")
            .unwrap()
            .set,
        model: crate::agent::ModelChoice {
            model: Some("opus".into()),
            effort: Some("high".into()),
        },
        args: vec!["--fast".into()],
        fork: Some("parent-session".into()),
        issue: Some(LaunchIssue {
            url: "https://forge.example/team/repo/issues/7".parse().unwrap(),
            title: "Fix the login".into(),
            labels: vec!["bug".into()],
        }),
        run: Some(
            "https://forge.example/team/repo/actions/runs/3"
                .parse()
                .unwrap(),
        ),
        env: Some([("PLACE".to_owned(), "beside".to_owned())].into()),
        here: true,
        source: Some(LaunchSource::Trigger {
            name: "nightly".into(),
        }),
        ..claude()
    };
    let pr = Some("8".parse().unwrap());
    QueuedStart { launch, pr }
}

/// The plan `entry`'s row keeps, and the start the row gives back.
async fn stored(fixture: &Fixture, entry: &QueueEntry) -> (serde_json::Value, QueuedStart) {
    let id = entry.id.clone();
    (fixture.agents.store)
        .run(move |db| {
            let read = "SELECT * FROM agent_queue WHERE id=?1";
            Ok(db.query_row(read, [id], |row| {
                let plan: String = row.get("plan")?;
                Ok((plan, super::planned_start(row)?))
            })?)
        })
        .await
        .map(|(plan, start)| (serde_json::from_str(&plan).unwrap(), start))
        .unwrap()
}

#[tokio::test]
async fn a_launch_row_gives_back_the_start_it_keeps_in_its_columns_and_plan() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("work").await;
    let mut request = queued_in(&workspace, claude());
    request.start = everything();
    let daemon = &*fixture.daemon;
    let admission = fixture.agents.queue_add(daemon, request).await.unwrap();

    let (plan, start) = stored(&fixture, &admission.entry.unwrap()).await;

    let written = serde_json::to_value(everything()).unwrap();
    assert_eq!(serde_json::to_value(start).unwrap(), written);
    let launch = &plan["launch"];
    for column in ["agent", "mode", "narrowing", "source"] {
        assert_eq!(launch.get(column), None, "{plan}");
    }
    assert_eq!(launch["issue"], "https://forge.example/team/repo/issues/7");
}
