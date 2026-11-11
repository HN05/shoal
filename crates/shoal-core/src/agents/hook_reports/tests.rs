use super::*;

fn idle() -> Report {
    Report::State {
        state: AgentState::Idle,
        session: None,
        turn_end: Some(TurnEnd::default()),
    }
}

fn space() -> SpaceId {
    SpaceId::from("space-1".to_owned())
}

/// What `left` finds, each report handled.
fn handle_left(paths: &Paths) -> Vec<Option<Kept>> {
    let left = find(paths, &space()).unwrap().left;
    left.into_iter()
        .map(|left| {
            let kept = left.kept.clone();
            left.handled().unwrap();
            kept
        })
        .collect()
}

#[test]
fn a_delivered_report_leaves_only_its_mark() {
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(temp.path());

    let first = Sending::keep(&paths, &space(), idle(), None).unwrap();
    let second = Sending::keep(&paths, &space(), idle(), None).unwrap();
    let made = second.kept.at;
    second.delivered();
    // The earlier report delivered later leaves the later mark.
    first.delivered();

    assert_eq!(handle_left(&paths), Vec::new());
    assert_eq!(last_delivered(&paths, &space()), Some(made));
}

#[test]
fn a_report_in_flight_stays_until_its_hook_ends() {
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(temp.path());
    let execution = Some(ExecutionId::from("execution-1".to_owned()));

    let sending = Sending::keep(&paths, &space(), idle(), execution.clone()).unwrap();
    assert_eq!(handle_left(&paths), Vec::new());
    assert_eq!(
        find(&paths, &space()).unwrap().sending,
        Some(sending.made())
    );
    assert_eq!(spaces(&paths).unwrap(), vec![space()]);
    // A hook its time limit kills ends without a word.
    drop(sending);

    let left = handle_left(&paths);
    let [Some(kept)] = left.as_slice() else {
        panic!("one report is left: {left:?}");
    };
    assert_eq!((&kept.report, &kept.execution), (&idle(), &execution));
    assert_eq!(kept.error, None);
    assert_eq!(handle_left(&paths), Vec::new());
    assert_eq!(last_delivered(&paths, &space()), None);
}

#[test]
fn a_report_left_unhandled_stays() {
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(temp.path());
    Sending::keep(&paths, &space(), idle(), None)
        .unwrap()
        .failed("daemon protocol mismatch".into());

    let held = find(&paths, &space()).unwrap().left;
    assert_eq!(held.len(), 1);
    // Another look finds nothing while the daemon holds it.
    assert!(find(&paths, &space()).unwrap().left.is_empty());
    drop(held);

    let left = handle_left(&paths);
    let [Some(kept)] = left.as_slice() else {
        panic!("one report is left: {left:?}");
    };
    assert_eq!(kept.error.as_deref(), Some("daemon protocol mismatch"));
}

#[test]
fn an_unreadable_report_is_left_as_none() {
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(temp.path());
    let directory = paths.hook_reports().join(space());
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("1-1.json"), "{").unwrap();
    fs::write(directory.join(".written"), "{").unwrap();

    assert_eq!(handle_left(&paths), vec![None]);
    assert!(directory.join(".written").exists());

    forget(&paths, &space()).unwrap();
    assert!(spaces(&paths).unwrap().is_empty());
}
