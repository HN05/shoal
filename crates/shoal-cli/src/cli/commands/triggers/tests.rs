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
        workspaces_dir: "/tmp/shoal/repo".into(),
        contribution: false,
        shared_url: None,
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

#[test]
fn a_sample_request_takes_the_payload_file_and_name_value_headers() {
    let temp = tempfile::tempdir().unwrap();
    let payload = temp.path().join("issue.json");
    std::fs::write(&payload, "{\"issue\":{}}").unwrap();

    let headers = ["X-Forgejo-Event=issues".to_owned(), "X-Empty=".to_owned()];
    let sample = sample_request(Some(&payload), &headers).unwrap().unwrap();

    assert_eq!(sample.body, "{\"issue\":{}}");
    assert_eq!(
        sample.headers,
        [
            ("X-Forgejo-Event".to_owned(), "issues".to_owned()),
            ("X-Empty".to_owned(), String::new())
        ]
    );
    assert!(sample_request(None, &[]).unwrap().is_none());
    assert!(sample_request(None, &["X-Forgejo-Event: issues".to_owned()]).is_err());
}
