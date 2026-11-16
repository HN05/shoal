use super::*;
use shoal_protocol::triggers::{RunOrigin, SourceKind};

fn status(name: &str, repository: Option<&str>, last_run: Option<TriggerRun>) -> TriggerStatus {
    TriggerStatus {
        name: name.into(),
        repository: repository.map(Into::into),
        source: SourceKind::Timer,
        paused: false,
        next_at: Some(UnixTime::from_seconds(0)),
        last_run,
    }
}

#[test]
fn rows_name_where_each_trigger_is_and_what_its_last_run_did() {
    let repositories = [Repository {
        id: "r1".into(),
        path: "/work/app".into(),
        source: "/work/app".into(),
        last_used: 0,
        name: Some("app".into()),
        workspaces_dir: None,
        contribution: false,
    }];
    let queued = TriggerRun {
        at: UnixTime::now(),
        origin: RunOrigin::Source,
        outcome: RunOutcome::Queued,
        reason: None,
        space_id: Some("s1".into()),
        queue_entry: None,
    };
    let triggers = [
        status("nightly", None, None),
        status("triage", Some("r1"), Some(queued)),
    ];

    let rows = trigger_rows(&triggers, &repositories, Palette::stdout(true));

    assert!(rows[0].contains("global"), "{rows:?}");
    assert!(rows[1].contains(" app "), "{rows:?}");
    assert!(
        rows[1].ends_with("queued, waiting for space s1"),
        "{rows:?}"
    );
}
