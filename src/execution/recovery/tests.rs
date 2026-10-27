use super::*;

#[test]
fn legacy_records_remain_readable_and_pressure_handoffs_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let old: Record = serde_json::from_str(r#"{"agent":"codex"}"#).unwrap();
    assert!(old.stop_reason.is_none());
    assert_eq!(old.stage, None);
    assert_eq!(old.turn, None);
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let path = save_record(
        &paths,
        "workspace",
        "execution",
        "codex",
        Some("critical memory pressure"),
        None,
    )
    .unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let record: Record = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(record.agent, "codex");
    assert_eq!(
        record.stop_reason.as_deref(),
        Some("critical memory pressure")
    );
    assert_eq!(record.stage, Some(Stage::Stopping));
    assert!(pending(&paths, "workspace").unwrap());
    let recovery = Recovery {
        agent: "codex".into(),
        command: vec![],
        automatic: false,
        handoff_delivered: false,
    };
    recovery
        .save(
            &paths,
            "workspace",
            "execution",
            None,
            Some("w1:p2".into()),
            Restorer::User,
        )
        .unwrap();
    save_record(&paths, "workspace", "execution", "codex", None, None).unwrap();
    let record: Record = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        record.stop_reason.as_deref(),
        Some("critical memory pressure")
    );
    assert_eq!(record.place.as_deref(), Some("w1:p2"));
    // The daemon's late handoff cannot undo the wrapper's stop.
    assert_eq!(record.stage, Some(Stage::Stopped));
}

#[test]
fn only_a_daemon_saved_stop_counts_as_undelivered() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let path = record_path(&paths, "workspace", "execution");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // A record from before stages may be the wrapper's own.
    std::fs::write(&path, r#"{"agent":"codex"}"#).unwrap();
    assert!(!discard_undelivered(&paths, "workspace", "execution").unwrap());
    assert!(path.is_file());
    std::fs::remove_file(&path).unwrap();
    save_record(&paths, "workspace", "execution", "codex", None, None).unwrap();
    assert!(discard_undelivered(&paths, "workspace", "execution").unwrap());
    assert!(!path.exists());
    assert!(!discard_undelivered(&paths, "workspace", "execution").unwrap());
}

#[test]
fn a_restored_workspace_saves_the_removed_ones_stopped_agents() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let recovery = Recovery {
        agent: "claude".into(),
        command: vec![],
        automatic: true,
        handoff_delivered: false,
    };
    let wrapper = identity::capture(std::process::id())
        .unwrap()
        .expect("this process has an identity");
    recovery
        .save(
            &paths,
            "removed",
            "execution",
            None,
            None,
            Restorer::Wrapper(wrapper),
        )
        .unwrap();
    save_command(&paths, "removed", "command", &[OsString::from("true")]).unwrap();

    let agents = stopped_agents(&paths, "removed").unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].execution, "execution");
    // The wrapper has exited, so the restored workspace waits for `shoal resume`.
    assert_eq!(agents[0].record.waiting, None);
    save_stopped_agents(&paths, "restored", &agents).unwrap();
    let mut saved = Saved::load(&paths, "restored").unwrap();
    saved.retain_unattended();
    assert!(saved.commands.is_empty());
    assert_eq!(saved.agents.len(), 1);
    let path = &saved.agents[0].1;
    assert_eq!(Record::load(path).unwrap().unwrap(), agents[0].record);
    assert_eq!(
        std::fs::metadata(path).unwrap().modified().unwrap(),
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(agents[0].stopped_at)
    );
}

#[test]
fn commands_with_arguments_that_are_not_utf8_are_not_recorded() {
    use std::os::unix::ffi::OsStringExt;
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let argv = [OsString::from("cat"), OsString::from_vec(vec![0xff])];
    assert!(save_command(&paths, "workspace", "execution", &argv).is_err());
    assert!(!pending(&paths, "workspace").unwrap());
}

#[test]
fn the_first_saved_turn_stays_through_later_saves() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    save_turn(&paths, "workspace", "execution", AgentState::Idle).unwrap();
    assert!(!pending(&paths, "workspace").unwrap());

    save_record(&paths, "workspace", "execution", "codex", None, None).unwrap();
    save_turn(&paths, "workspace", "execution", AgentState::Working).unwrap();
    save_turn(&paths, "workspace", "execution", AgentState::Idle).unwrap();
    let recovery = Recovery {
        agent: "codex".into(),
        command: vec![],
        automatic: false,
        handoff_delivered: false,
    };
    let path = recovery
        .save(&paths, "workspace", "execution", None, None, Restorer::User)
        .unwrap();
    let record = Record::load(&path).unwrap().unwrap();
    assert_eq!(record.turn, Some(AgentState::Working));
    assert_eq!(record.stage, Some(Stage::Stopped));
}

#[test]
fn a_restore_request_stays_until_the_daemon_clears_it() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let recovery = Recovery {
        agent: "claude".into(),
        command: vec![],
        automatic: true,
        handoff_delivered: false,
    };
    let env = BTreeMap::from([("HERDR_ENV".to_owned(), "1".to_owned())]);
    recovery
        .save(
            &paths,
            "workspace",
            "execution",
            Some("its terminal closed"),
            None,
            Restorer::Queue(env.clone()),
        )
        .unwrap();
    // The daemon's later save of the turn keeps the request.
    save_turn(&paths, "workspace", "execution", AgentState::Working).unwrap();
    let requests = restore_requests(&paths, "workspace").unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "execution");
    assert_eq!(requests[0].1.restore_request.as_ref(), Some(&env));

    clear_restore_request(&paths, "workspace", "execution").unwrap();
    assert!(restore_requests(&paths, "workspace").unwrap().is_empty());
    let record = Record::load(&record_path(&paths, "workspace", "execution"))
        .unwrap()
        .expect("the record stays for shoal resume");
    assert_eq!(record.stop_reason.as_deref(), Some("its terminal closed"));
    // A record already resumed has nothing to clear.
    clear_restore_request(&paths, "workspace", "gone").unwrap();
}
