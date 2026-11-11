use super::*;
use crate::client::space_ids::SpaceId;
use crate::time::UnixTime;

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
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
        &crate::execution::TrackedAgent::named("codex"),
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
    assert!(pending(&paths, &SpaceId::from("workspace")).unwrap());
    let recovery = Recovery {
        narrowing: Default::default(),
        mode: "worker".into(),
        model: Default::default(),
        agent: "codex".into(),
        command: vec![],
        automatic: false,
        handoff_delivered: false,
        proxy: None,
    };
    recovery
        .save(
            &paths,
            &SpaceId::from("workspace"),
            &ExecutionId::from("execution"),
            None,
            Some(herdr_pane("w1:p2")),
            Restorer::User,
        )
        .unwrap();
    save_record(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
        &crate::execution::TrackedAgent::named("codex"),
        None,
        None,
    )
    .unwrap();
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
    let path = record_path(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
    );
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // A record from before stages may be the wrapper's own.
    std::fs::write(&path, r#"{"agent":"codex"}"#).unwrap();
    assert!(
        !discard_undelivered(
            &paths,
            &SpaceId::from("workspace"),
            &ExecutionId::from("execution")
        )
        .unwrap()
    );
    assert!(path.is_file());
    std::fs::remove_file(&path).unwrap();
    save_record(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
        &crate::execution::TrackedAgent::named("codex"),
        None,
        None,
    )
    .unwrap();
    assert!(
        discard_undelivered(
            &paths,
            &SpaceId::from("workspace"),
            &ExecutionId::from("execution")
        )
        .unwrap()
    );
    assert!(!path.exists());
    assert!(
        !discard_undelivered(
            &paths,
            &SpaceId::from("workspace"),
            &ExecutionId::from("execution")
        )
        .unwrap()
    );
}

#[test]
fn a_restored_workspace_saves_the_removed_ones_stopped_agents() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let recovery = Recovery {
        narrowing: Default::default(),
        mode: "worker".into(),
        model: Default::default(),
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
            &SpaceId::from("removed"),
            &ExecutionId::from("execution"),
            None,
            None,
            Restorer::Wrapper(wrapper),
        )
        .unwrap();
    save_command(
        &paths,
        &SpaceId::from("removed"),
        &ExecutionId::from("command"),
        &[OsString::from("true")],
    )
    .unwrap();

    let agents = stopped_agents(&paths, &SpaceId::from("removed")).unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].execution, "execution");
    // The wrapper has exited, so the restored workspace waits for `shoal resume`.
    assert_eq!(agents[0].record.waiting, None);
    save_stopped_agents(&paths, &SpaceId::from("restored"), &agents).unwrap();
    let mut saved = Saved::load(&paths, &SpaceId::from("restored")).unwrap();
    saved.retain_unattended();
    assert!(saved.commands.is_empty());
    assert_eq!(saved.agents.len(), 1);
    let path = &saved.agents[0].1;
    assert_eq!(Record::load(path).unwrap().unwrap(), agents[0].record);
    assert_eq!(
        std::fs::metadata(path).unwrap().modified().unwrap(),
        agents[0].stopped_at.system_time()
    );
}

#[test]
fn only_agents_outside_a_waiting_wrapper_receive_the_stopped_commands() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let recovery = Recovery {
        narrowing: Default::default(),
        mode: "worker".into(),
        model: Default::default(),
        agent: "claude".into(),
        command: vec![],
        automatic: true,
        handoff_delivered: false,
        proxy: None,
    };
    let wrapper = identity::capture(std::process::id())
        .unwrap()
        .expect("this process has an identity");
    let save = |execution: &ExecutionId, restorer| {
        recovery
            .save(
                &paths,
                &SpaceId::from("workspace"),
                execution,
                None,
                None,
                restorer,
            )
            .unwrap()
    };
    save(&ExecutionId::from("waiting"), Restorer::Wrapper(wrapper));
    let saved = Saved::load(&paths, &SpaceId::from("workspace")).unwrap();
    assert!(waits_in_wrapper(&saved.agents[0].1));
    assert!(!saved.restores_outside_wrappers());

    save(&ExecutionId::from("stopped"), Restorer::User);
    let mut saved = Saved::load(&paths, &SpaceId::from("workspace")).unwrap();
    assert!(saved.restores_outside_wrappers());
    saved.retain_unattended();
    let kept: Vec<_> = saved.agents.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(kept, ["stopped"]);
}

#[test]
fn commands_with_arguments_that_are_not_utf8_are_not_recorded() {
    use std::os::unix::ffi::OsStringExt;
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let argv = [OsString::from("cat"), OsString::from_vec(vec![0xff])];
    assert!(
        save_command(
            &paths,
            &SpaceId::from("workspace"),
            &ExecutionId::from("execution"),
            &argv
        )
        .is_err()
    );
    assert!(!pending(&paths, &SpaceId::from("workspace")).unwrap());
}

#[test]
fn the_first_saved_turn_stays_through_later_saves() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    save_turn(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
        AgentState::Idle,
    )
    .unwrap();
    assert!(!pending(&paths, &SpaceId::from("workspace")).unwrap());

    save_record(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
        &crate::execution::TrackedAgent::named("codex"),
        None,
        None,
    )
    .unwrap();
    save_turn(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
        AgentState::Working,
    )
    .unwrap();
    save_turn(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
        AgentState::Idle,
    )
    .unwrap();
    let recovery = Recovery {
        narrowing: Default::default(),
        mode: "worker".into(),
        model: Default::default(),
        agent: "codex".into(),
        command: vec![],
        automatic: false,
        handoff_delivered: false,
        proxy: None,
    };
    let path = recovery
        .save(
            &paths,
            &SpaceId::from("workspace"),
            &ExecutionId::from("execution"),
            None,
            None,
            Restorer::User,
        )
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
        narrowing: Default::default(),
        mode: "worker".into(),
        model: Default::default(),
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
            &SpaceId::from("workspace"),
            &ExecutionId::from("execution"),
            Some("its terminal closed"),
            None,
            Restorer::Queue(env.clone()),
        )
        .unwrap();
    assert_eq!(
        restore_requests(&paths, &SpaceId::from("workspace")).unwrap(),
        [(ExecutionId::from("execution"), env)]
    );
    // The request is no stopped work of its own.
    assert_eq!(
        agent_records(&paths, &SpaceId::from("workspace")).unwrap(),
        ["execution"]
    );

    forget_restore_request(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
    )
    .unwrap();
    assert!(
        restore_requests(&paths, &SpaceId::from("workspace"))
            .unwrap()
            .is_empty()
    );
    let record = Record::load(&record).unwrap().expect("the record stays");
    assert_eq!(record.stop_reason.as_deref(), Some("its terminal closed"));
    forget_restore_request(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
    )
    .unwrap();
}

#[test]
fn each_agent_keeps_a_last_session_and_a_single_earlier_one_gets_a_file() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let state = paths.workspace_state(&SpaceId::from("workspace"));
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(
        state.join("last-session.json"),
        r#"{"agent":"codex","session":"earlier"}"#,
    )
    .unwrap();
    LastSession::save(
        &paths,
        &SpaceId::from("workspace"),
        &crate::execution::TrackedAgent::named("claude code"),
        &ExecutionId::from("execution"),
    )
    .unwrap();
    let mut saved = Saved::load(&paths, &SpaceId::from("workspace")).unwrap();
    assert!(!state.join("last-session.json").exists());
    assert_eq!(saved.sessions.len(), 2);
    assert!(!saved.is_empty() && !saved.is_unfinished());
    assert!(!pending(&paths, &SpaceId::from("workspace")).unwrap());
    let mut agents = LastSession::all(&paths, &SpaceId::from("workspace"))
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
    saved.retain_execution(&ExecutionId::from("execution"), false);
    assert_eq!(
        saved.sessions,
        [(
            "execution".into(),
            LastSession::path(
                &paths,
                &SpaceId::from("workspace"),
                &ExecutionId::from("execution")
            )
        )]
    );
    LastSession::forget_all(&paths, &SpaceId::from("workspace")).unwrap();
    assert!(
        Saved::load(&paths, &SpaceId::from("workspace"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_stopped_agent_kept_as_a_last_session_keeps_its_session_and_stop() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let record = write_record(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
        Record {
            session: Some("stopped".into()),
            ..Record::new(
                &crate::execution::TrackedAgent::named("claude"),
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
    keep_as_last_session(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
        &record,
        None,
    )
    .unwrap();
    assert!(!record.exists());
    assert!(!pending(&paths, &SpaceId::from("workspace")).unwrap());
    assert_eq!(
        LastSession::all(&paths, &SpaceId::from("workspace")).unwrap(),
        [LastSession {
            narrowing: Default::default(),
            mode: "worker".into(),
            model: Default::default(),
            execution: "execution".into(),
            agent: "claude".into(),
            session: Some("stopped".into()),
            moved: false,
            copied: None,
            origin: Some(herdr_pane("w1:p2")),
            stopped_by: None,
            stop: Some(Stop {
                reason: Some("its terminal closed".into()),
                at: Some(UnixTime::from_seconds(1_800_000_000)),
            }),
            converted: false,
        }]
    );
    let kept = LastSession::path(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
    );
    assert_eq!(saved_place(&kept), Some(herdr_pane("w1:p2")));
    assert_eq!(
        saved_path(
            &paths,
            &SpaceId::from("workspace"),
            &ExecutionId::from("execution")
        ),
        kept
    );
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
    let unknown = serde_json::json!({"agent": "codex", "origin": {"host": "screen"}});
    assert!(serde_json::from_value::<Record>(unknown).is_err());
}

#[test]
fn a_restored_session_learns_why_and_when_it_stopped_and_what_the_stop_ended() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let path = save_record(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
        &crate::execution::TrackedAgent::named("claude"),
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
            at: Some(UnixTime::from_seconds(1_800_000_000)),
        }
    );
    let at = UnixTime::from_seconds(1_800_000_000).local_minutes();
    let prompt = resumed_prompt(Some(&stop), Some("handoff"));
    assert!(prompt.contains(&at), "{prompt}");
    assert!(prompt.contains("critical memory pressure"), "{prompt}");
    assert!(prompt.ends_with("\n\nhandoff"), "{prompt}");
    // A last session, which exited on its own, has no stop to describe.
    assert_ne!(
        resumed_prompt(Some(&Stop::default()), None),
        resumed_prompt(None, None)
    );
}

#[test]
fn copied_records_keep_their_mark_where_they_move() {
    let record: Record = serde_json::from_str(r#"{"agent":"claude","session":"s1"}"#).unwrap();
    assert_eq!(record.copied, None);
    let copied = Record {
        copied: Some(Copied {
            earlier_files: true,
        }),
        ..record
    };
    let saved: Record = serde_json::from_str(&serde_json::to_string(&copied).unwrap()).unwrap();
    assert_eq!(saved, copied);
    let moved = copied.moved();
    assert_eq!(moved.session, None);
    assert_eq!(moved.copied, saved.copied);
}

#[test]
fn copied_agents_are_told_the_workspace_is_a_copy() {
    let forked = copied_prompt(Copied::default(), true, Some("handoff"));
    assert!(forked.contains("copied this workspace"), "{forked}");
    assert!(forked.contains("a fork of its agent's"), "{forked}");
    assert!(!forked.contains("earlier checkpoint"), "{forked}");
    assert!(forked.ends_with("\n\nhandoff"), "{forked}");
    let earlier = copied_prompt(
        Copied {
            earlier_files: true,
        },
        false,
        None,
    );
    assert!(
        earlier.contains("session is not available here"),
        "{earlier}"
    );
    assert!(earlier.contains("earlier checkpoint"), "{earlier}");
}

#[test]
fn the_space_that_asked_for_a_stop_outlasts_the_wrappers_save_and_shoal_stop() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let path = save_record_stopped_by(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
        &crate::execution::TrackedAgent::named("codex"),
        Some("restart"),
        None,
        Some(&SpaceId::from("parent")),
    )
    .unwrap();
    let recovery = Recovery {
        narrowing: Default::default(),
        mode: "worker".into(),
        model: Default::default(),
        agent: "codex".into(),
        command: vec![],
        automatic: false,
        handoff_delivered: false,
        proxy: None,
    };
    // The wrapper completes the record without knowing who asked.
    recovery
        .save(
            &paths,
            &SpaceId::from("workspace"),
            &ExecutionId::from("execution"),
            None,
            None,
            Restorer::User,
        )
        .unwrap();
    let stopper = stopped_by(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
    )
    .unwrap();
    assert_eq!(stopper.as_ref().map(SpaceId::as_str), Some("parent"));
    // A stop that names no stopper keeps the one the record names.
    keep_as_last_session(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
        &path,
        None,
    )
    .unwrap();
    assert!(!path.exists());
    let stopper = stopped_by(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
    )
    .unwrap();
    assert_eq!(stopper.as_ref().map(SpaceId::as_str), Some("parent"));
}

#[test]
fn a_permission_change_reaches_each_saved_agent_and_keeps_the_rest() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let workspace = SpaceId::from("workspace");
    let agent = crate::execution::TrackedAgent {
        narrowing: crate::config::permissions::PermissionsConfig::assigned(&[
            "push=propose".into(),
            "merge=propose".into(),
        ])
        .unwrap(),
        ..crate::execution::TrackedAgent::named("claude")
    };
    save_record(
        &paths,
        &workspace,
        &ExecutionId::from("stopped"),
        &agent,
        Some("asked"),
        None,
    )
    .unwrap();
    LastSession::save(&paths, &workspace, &agent, &ExecutionId::from("last")).unwrap();
    let change = crate::config::permissions::PermissionChange {
        set: crate::config::permissions::PermissionsConfig::assigned(&["api_writes=deny".into()])
            .unwrap(),
        dropped: vec![crate::config::permissions::PermissionKey::Merge],
    };
    // The key it does not name keeps its value.
    let replacement = crate::config::permissions::PermissionsConfig::assigned(&[
        "push=propose".into(),
        "api_writes=deny".into(),
    ])
    .unwrap();
    change_permissions(&[Saved::load(&paths, &workspace).unwrap()], &change).unwrap();
    let record = Record::load(&record_path(
        &paths,
        &workspace,
        &ExecutionId::from("stopped"),
    ))
    .unwrap()
    .unwrap();
    assert_eq!(
        (record.narrowing, record.stop_reason.as_deref()),
        (replacement, Some("asked"))
    );
    let last = LastSession::all(&paths, &workspace).unwrap();
    assert_eq!(
        last.iter()
            .map(|last| (last.execution.as_str(), last.narrowing))
            .collect::<Vec<_>>(),
        [("last", replacement)]
    );

    // A record a restore claimed is refused, and none changes, in any
    // workspace.
    let other = SpaceId::from("other");
    LastSession::save(&paths, &other, &agent, &ExecutionId::from("other-last")).unwrap();
    let record = record_path(&paths, &workspace, &ExecutionId::from("stopped"));
    let _restoring = claim(&record).unwrap();
    let saved = [&other, &workspace].map(|id| Saved::load(&paths, id).unwrap());
    assert!(change_permissions(&saved, &change).is_err());
    let last = LastSession::all(&paths, &workspace).unwrap();
    assert_eq!(last[0].narrowing, replacement);
    let other_last = LastSession::all(&paths, &other).unwrap();
    assert_eq!(other_last[0].narrowing, agent.narrowing);
}

#[test]
fn a_converted_workspaces_agent_is_told_its_place_and_the_stopped_commands() {
    let mut workspace = Workspace::new_record(
        "repo".into(),
        "talk".into(),
        "/work/fix-login".into(),
        Some("fix-login".into()),
        crate::state::WorkspaceState::Ready,
    );
    let url = "https://forge.example/team/repo/issues/7";
    workspace.links.issue = Some(url.into());
    workspace.links.issue_title = Some("Fix the login".into());

    let told = converted_subject(&workspace, true, Some("handoff"));

    let text = &told.text;
    assert!(
        text.contains("/work/fix-login") && text.contains("fix-login"),
        "{text}"
    );
    assert!(text.ends_with("\n\nhandoff"), "{text}");
    // Its mode's instructions name the issue it was linked to as a chat.
    let issue = told.issue.expect("the linked issue's fields");
    assert_eq!(
        (issue.number, issue.title.as_str(), issue.url.as_str()),
        (7, "Fix the login", url)
    );
}
