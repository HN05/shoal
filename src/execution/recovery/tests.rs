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
        proxy: None,
    };
    recovery
        .save(
            &paths,
            "workspace",
            "execution",
            None,
            Some(herdr_pane("w1:p2")),
            Restorer::User,
        )
        .unwrap();
    save_record(&paths, "workspace", "execution", "codex", None, None).unwrap();
    let record: Record = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        record.stop_reason.as_deref(),
        Some("critical memory pressure")
    );
    assert_eq!(record.origin, Some(herdr_pane("w1:p2")));
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
        proxy: None,
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
        proxy: None,
    };
    let path = recovery
        .save(&paths, "workspace", "execution", None, None, Restorer::User)
        .unwrap();
    let record = Record::load(&path).unwrap().unwrap();
    assert_eq!(record.turn, Some(AgentState::Working));
    assert_eq!(record.stage, Some(Stage::Stopped));
}

#[test]
fn a_restore_request_sits_beside_the_record_until_forgotten() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let recovery = Recovery {
        agent: "claude".into(),
        command: vec![],
        automatic: true,
        handoff_delivered: false,
        proxy: None,
    };
    let env = BTreeMap::from([("HERDR_ENV".to_owned(), "1".to_owned())]);
    let record = recovery
        .save(
            &paths,
            "workspace",
            "execution",
            Some("its terminal closed"),
            None,
            Restorer::Queue(env.clone()),
        )
        .unwrap();
    assert_eq!(
        restore_requests(&paths, "workspace").unwrap(),
        [("execution".to_owned(), env)]
    );
    // The request is no stopped work of its own.
    assert_eq!(agent_records(&paths, "workspace").unwrap(), ["execution"]);

    forget_restore_request(&paths, "workspace", "execution").unwrap();
    assert!(restore_requests(&paths, "workspace").unwrap().is_empty());
    let record = Record::load(&record).unwrap().expect("the record stays");
    assert_eq!(record.stop_reason.as_deref(), Some("its terminal closed"));
    forget_restore_request(&paths, "workspace", "execution").unwrap();
}

#[test]
fn each_agent_keeps_a_last_session_and_a_single_earlier_one_gets_a_file() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let state = paths.workspace_state("workspace");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(
        state.join("last-session.json"),
        r#"{"agent":"codex","session":"earlier"}"#,
    )
    .unwrap();
    LastSession::save(&paths, "workspace", "claude code", "execution").unwrap();
    let mut saved = Saved::load(&paths, "workspace").unwrap();
    assert!(!state.join("last-session.json").exists());
    assert_eq!(saved.sessions.len(), 2);
    assert!(!saved.is_empty() && !saved.is_unfinished());
    assert!(!pending(&paths, "workspace").unwrap());
    let mut agents = LastSession::all(&paths, "workspace")
        .unwrap()
        .into_iter()
        .map(|last| (last.agent, last.session))
        .collect::<Vec<_>>();
    agents.sort();
    assert_eq!(
        agents,
        [
            ("claude-code".into(), None),
            ("codex".into(), Some("earlier".into()))
        ]
    );
    saved.retain_execution("execution", false);
    assert_eq!(
        saved.sessions,
        [(
            "execution".into(),
            LastSession::path(&paths, "workspace", "execution")
        )]
    );
    LastSession::forget_all(&paths, "workspace").unwrap();
    assert!(Saved::load(&paths, "workspace").unwrap().is_empty());
}

#[test]
fn a_stopped_agent_kept_as_a_last_session_keeps_its_session_and_stop() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let record = write_record(
        &paths,
        "workspace",
        "execution",
        Record {
            session: Some("stopped".into()),
            ..Record::new(
                "claude",
                Some("its terminal closed"),
                Stage::Stopped,
                Some(herdr_pane("w1:p2")),
            )
        },
    )
    .unwrap();
    let stopped = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000);
    std::fs::File::open(&record)
        .unwrap()
        .set_modified(stopped)
        .unwrap();
    keep_as_last_session(&paths, "workspace", "execution", &record, None).unwrap();
    assert!(!record.exists());
    assert!(!pending(&paths, "workspace").unwrap());
    assert_eq!(
        LastSession::all(&paths, "workspace").unwrap(),
        [LastSession {
            execution: "execution".into(),
            agent: "claude".into(),
            session: Some("stopped".into()),
            moved: false,
            origin: Some(herdr_pane("w1:p2")),
            stopped_by: None,
            stop: Some(Stop {
                reason: Some("its terminal closed".into()),
                at: Some(1_800_000_000),
            }),
        }]
    );
    let kept = LastSession::path(&paths, "workspace", "execution");
    assert_eq!(saved_place(&kept), Some(herdr_pane("w1:p2")));
    assert_eq!(saved_path(&paths, "workspace", "execution"), kept);
}

fn herdr_pane(pane: &str) -> Origin {
    serde_json::from_value(serde_json::json!({"host": "herdr", "place": pane})).unwrap()
}

#[test]
fn a_pane_recorded_before_origins_is_a_herdr_place() {
    let record: Record =
        serde_json::from_value(serde_json::json!({"agent": "codex", "place": "w1:p2"})).unwrap();
    assert_eq!(record.origin, Some(herdr_pane("w1:p2")));
    let saved = serde_json::to_value(&record).unwrap();
    assert_eq!(
        saved["origin"],
        serde_json::json!({"host": "herdr", "place": "w1:p2"})
    );
    assert_eq!(serde_json::from_value::<Record>(saved).unwrap(), record);
    let unknown = serde_json::json!({"agent": "codex", "origin": {"host": "tmux"}});
    assert!(serde_json::from_value::<Record>(unknown).is_err());
}

#[test]
fn a_restored_session_learns_why_and_when_it_stopped_and_what_the_stop_ended() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let path = save_record(
        &paths,
        "workspace",
        "execution",
        "claude",
        Some("critical memory pressure"),
        None,
    )
    .unwrap();
    let stopped = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000);
    std::fs::File::open(&path)
        .unwrap()
        .set_modified(stopped)
        .unwrap();
    let record = Record::load(&path).unwrap().unwrap();
    let stop = Stop::of(&record, &path);
    assert_eq!(
        stop,
        Stop {
            reason: Some("critical memory pressure".into()),
            at: Some(1_800_000_000),
        }
    );
    let at = crate::time::local_minutes(1_800_000_000);
    assert_eq!(
        resumed_prompt(Some(&stop), Some("handoff")),
        format!(
            "This session stopped at {at} (critical memory pressure); its subagents and background commands ended. Continue working on the unfinished assignment from where you left off.\n\nhandoff"
        )
    );
    // A stop without a known reason or time still says what it ended.
    assert_eq!(
        resumed_prompt(Some(&Stop::default()), None),
        "This session stopped; its subagents and background commands ended. Continue working on the unfinished assignment from where you left off."
    );
    // A last session, which exited on its own, has no stop to describe.
    assert_eq!(
        resumed_prompt(None, None),
        "Shoal resumed this session. Continue working on the unfinished assignment from where you left off."
    );
}
