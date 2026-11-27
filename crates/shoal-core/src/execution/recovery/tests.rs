use super::permissions::change_permissions;
use super::*;
use crate::time::UnixTime;
use crate::{
    paths::{Paths, StateFiles as _},
    process::identity,
    state::AgentState,
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::sessions::Origin;
use std::{collections::BTreeMap, ffi::OsString};

#[test]
fn pressure_handoffs_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let path = save_record(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
        &shoal_protocol::executions::TrackedAgent::named("codex"),
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
        &shoal_protocol::executions::TrackedAgent::named("codex"),
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
fn the_wrappers_save_keeps_a_load_stops_mark() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let (space, execution) = (SpaceId::from("workspace"), ExecutionId::from("execution"));
    let agent = shoal_protocol::executions::TrackedAgent::named("codex");
    let reason = Some("critical memory pressure");
    let path = save_load_record(&paths, &space, &execution, &agent, reason, None).unwrap();
    let recovery = Recovery {
        narrowing: Default::default(),
        mode: "worker".into(),
        model: Default::default(),
        agent: "codex".into(),
        command: vec![],
        automatic: true,
        handoff_delivered: true,
        proxy: None,
    };
    recovery
        .save(&paths, &space, &execution, reason, None, Restorer::User)
        .unwrap();
    assert!(Record::load(&path).unwrap().unwrap().for_load);
}

#[test]
fn clearing_a_load_mark_waits_for_a_claimed_record() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let (space, execution) = (SpaceId::from("workspace"), ExecutionId::from("execution"));
    let agent = shoal_protocol::executions::TrackedAgent::named("codex");
    let path = save_load_record(&paths, &space, &execution, &agent, None, None).unwrap();
    let held = claim(&path).unwrap();
    assert!(clear_load_mark(&paths, &space, &execution).is_err());
    assert!(Record::load(&path).unwrap().unwrap().for_load);
    drop(held);
    clear_load_mark(&paths, &space, &execution).unwrap();
    assert!(!Record::load(&path).unwrap().unwrap().for_load);
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
    // A record without a stage may be the wrapper's own.
    std::fs::write(&path, r#"{"agent":"codex","mode":"worker"}"#).unwrap();
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
        &shoal_protocol::executions::TrackedAgent::named("codex"),
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
    let path = &saved.agents[0].path;
    assert_eq!(Record::load(path).unwrap().unwrap(), agents[0].record);
    assert_eq!(
        std::fs::metadata(path).unwrap().modified().unwrap(),
        agents[0].stopped_at.system_time()
    );
}

#[test]
fn saved_agents_carry_their_agent_stop_reason_and_turn() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let workspace = SpaceId::from("workspace");
    let save = |execution: &str, record: &str| {
        let path = record_path(&paths, &workspace, &ExecutionId::from(execution));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, record).unwrap();
    };
    save(
        "busy",
        r#"{"agent":"claude","stop_reason":"memory pressure","mode":"worker"}"#,
    );
    save("idle", r#"{"agent":"codex","turn":"idle","mode":"worker"}"#);
    let saved = Saved::load(&paths, &workspace).unwrap();
    let agents = (saved.agents.iter())
        .map(|agent| {
            let name = agent.agent.as_ref().map(ToString::to_string);
            (
                agent.id.as_str(),
                name,
                agent.stop_reason.as_deref(),
                agent.idle,
            )
        })
        .collect::<Vec<_>>();
    // A record without a turn counts as interrupted.
    assert_eq!(
        agents,
        [
            (
                "busy",
                Some("claude".into()),
                Some("memory pressure"),
                false
            ),
            ("idle", Some("codex".into()), None, true),
        ]
    );
    assert!(saved.agents.iter().all(|agent| agent.stopped_at.is_some()));
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
    assert!(saved.agents[0].waits_in_wrapper);
    assert!(!saved.restores_outside_wrappers());

    save(&ExecutionId::from("stopped"), Restorer::User);
    let mut saved = Saved::load(&paths, &SpaceId::from("workspace")).unwrap();
    assert!(saved.restores_outside_wrappers());
    saved.retain_unattended();
    let kept: Vec<_> = saved.agents.iter().map(|agent| agent.id.as_str()).collect();
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
        &shoal_protocol::executions::TrackedAgent::named("codex"),
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

/// A save interrupted before it requests the agent queue's restore leaves
/// no stopped record, which would otherwise read as a finished stop.
#[test]
fn a_stopped_record_follows_its_restore_request() {
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
    let (space, id) = (SpaceId::from("workspace"), ExecutionId::from("execution"));
    // A directory where the request goes makes its write fail.
    let record = record_path(&paths, &space, &id);
    let request = record.with_file_name("execution.restore-request.json");
    std::fs::create_dir_all(request.join("blocked")).unwrap();

    let env = BTreeMap::from([("HERDR_ENV".to_owned(), "1".to_owned())]);
    let saved = recovery.save(&paths, &space, &id, None, None, Restorer::Queue(env));

    assert!(saved.is_err());
    assert!(!record.exists());
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
fn each_agent_keeps_a_last_session() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let state = paths.space_state(&SpaceId::from("workspace"));
    std::fs::create_dir_all(&state).unwrap();
    let earlier = ExecutionId::from("earlier");
    std::fs::write(
        LastSession::path(&paths, &SpaceId::from("workspace"), &earlier),
        r#"{"execution":"earlier","agent":"codex","session":"earlier","mode":"worker"}"#,
    )
    .unwrap();
    LastSession::save(
        &paths,
        &SpaceId::from("workspace"),
        &shoal_protocol::executions::TrackedAgent::named("claude code"),
        &ExecutionId::from("execution"),
        None,
    )
    .unwrap();
    let mut saved = Saved::load(&paths, &SpaceId::from("workspace")).unwrap();
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
    saved.retain_execution(&ExecutionId::from("execution"));
    let kept = (saved.sessions.iter()).map(|last| (last.id.as_str(), &last.path));
    assert_eq!(
        kept.collect::<Vec<_>>(),
        [(
            "execution",
            &LastSession::path(
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

/// A staged last session outlives the discarding of the space's saved work,
/// replaces the session there once committed, and leaves nothing once
/// dropped.
#[test]
fn a_staged_last_session_counts_only_once_committed() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let space = SpaceId::from("workspace");
    let execution = ExecutionId::from("execution");
    let agent = shoal_protocol::executions::TrackedAgent::named("claude");
    LastSession::save(&paths, &space, &agent, &execution, None).unwrap();
    let taking = LastSession {
        agent: "codex".into(),
        ..LastSession::all(&paths, &space).unwrap().remove(0)
    };

    let staged = taking.stage(&paths, &space).unwrap();
    let agents = |paths| LastSession::all(paths, &space).unwrap();
    assert_eq!(agents(&paths)[0].agent.name(), "claude");
    LastSession::forget_all(&paths, &space).unwrap();
    assert!(agents(&paths).is_empty());
    staged.commit().unwrap();
    assert_eq!(agents(&paths)[0].agent.name(), "codex");

    drop(taking.stage(&paths, &space).unwrap());
    let files = std::fs::read_dir(paths.space_state(&space)).unwrap();
    assert_eq!(files.count(), 1);
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
                &shoal_protocol::executions::TrackedAgent::named("claude"),
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
            stop: Some(Stop {
                reason: Some("its terminal closed".into()),
                at: Some(UnixTime::from_seconds(1_800_000_000)),
            }),
            turn: None,
            converted: false,
            handoff: None,
        }]
    );
    let kept = LastSession::path(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
    );
    let saved = Saved::load(&paths, &SpaceId::from("workspace")).unwrap();
    assert_eq!(saved.sessions[0].origin, Some(herdr_pane("w1:p2")));
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
fn a_restored_session_learns_why_and_when_it_stopped_and_what_the_stop_ended() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let path = save_record(
        &paths,
        &SpaceId::from("workspace"),
        &ExecutionId::from("execution"),
        &shoal_protocol::executions::TrackedAgent::named("claude"),
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
    // A stop whose turn is unknown came mid-turn.
    let at = UnixTime::from_seconds(1_800_000_000).local_minutes();
    let stopped = Ending {
        stop: Some(&stop),
        turn: None,
    };
    let prompt = resumed_prompt(stopped, Some("handoff")).unwrap();
    assert!(prompt.contains(&at), "{prompt}");
    assert!(prompt.contains("critical memory pressure"), "{prompt}");
    assert!(prompt.ends_with("\n\nhandoff"), "{prompt}");
    // Without a stop to describe, it is still told to continue.
    let unrecorded = resumed_prompt(Ending::default(), None).unwrap();
    assert!(unrecorded.starts_with("Continue"), "{unrecorded}");
}

#[test]
fn a_session_that_ended_between_turns_is_told_only_its_handoff() {
    let stop = Stop::default();
    for stop in [Some(&stop), None] {
        let between = Ending {
            stop,
            turn: Some(AgentState::Idle),
        };
        assert_eq!(resumed_prompt(between, None), None);
        assert_eq!(
            resumed_prompt(between, Some("handoff")).as_deref(),
            Some("handoff")
        );
        // An agent that exited or stopped mid-turn is told to continue.
        let working = Ending {
            stop,
            turn: Some(AgentState::Working),
        };
        assert!(resumed_prompt(working, None).is_some());
    }
}

#[test]
fn a_last_session_keeps_the_turn_its_agent_ended_in() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let (space, execution) = (SpaceId::from("workspace"), ExecutionId::from("execution"));
    let agent = shoal_protocol::executions::TrackedAgent::named("claude");
    let turn = Some(AgentState::Idle);
    let path = save_record(&paths, &space, &execution, &agent, None, turn).unwrap();
    let record = Record::load(&path).unwrap().unwrap();
    let last = record.into_last_session(execution, &path);
    assert!(!last.ending().mid_turn());
    // One that exited on its own keeps the turn the daemon gives it.
    let exited = ExecutionId::from("exited");
    let working = Some(AgentState::Working);
    LastSession::save(&paths, &space, &agent, &exited, working).unwrap();
    let saved = LastSession::load(&LastSession::path(&paths, &space, &exited));
    assert!(saved.unwrap().unwrap().ending().mid_turn());
}

#[test]
fn copied_records_keep_their_mark_where_they_move() {
    let record: Record =
        serde_json::from_str(r#"{"agent":"claude","session":"s1","mode":"worker"}"#).unwrap();
    assert_eq!(record.copied, None);
    let copied = Record {
        copied: Some(Copied {
            earlier_files: true,
            held_elsewhere: false,
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
fn a_permission_change_reaches_each_saved_agent_and_keeps_the_rest() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let workspace = SpaceId::from("workspace");
    let agent = shoal_protocol::executions::TrackedAgent {
        narrowing: crate::config::permissions::PermissionsConfig::assigned(&[
            "push=propose".into(),
            "merge=propose".into(),
        ])
        .unwrap(),
        ..shoal_protocol::executions::TrackedAgent::named("claude")
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
    LastSession::save(&paths, &workspace, &agent, &ExecutionId::from("last"), None).unwrap();
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
    LastSession::save(
        &paths,
        &other,
        &agent,
        &ExecutionId::from("other-last"),
        None,
    )
    .unwrap();
    let record = record_path(&paths, &workspace, &ExecutionId::from("stopped"));
    let _restoring = claim(&record).unwrap();
    let saved = [&other, &workspace].map(|id| Saved::load(&paths, id).unwrap());
    assert!(change_permissions(&saved, &change).is_err());
    let last = LastSession::all(&paths, &workspace).unwrap();
    assert_eq!(last[0].narrowing, replacement);
    let other_last = LastSession::all(&paths, &other).unwrap();
    assert_eq!(other_last[0].narrowing, agent.narrowing);
}

/// An agent the OOM killer ended is told of it, between turns too, before
/// its handoff.
#[test]
fn a_session_the_oom_killer_ended_is_told_of_it() {
    let stop = Stop {
        reason: Some(OOM_REASON.into()),
        at: None,
    };
    for turn in [AgentState::Idle, AgentState::Working] {
        let ending = Ending {
            stop: Some(&stop),
            turn: Some(turn),
        };
        let prompt = resumed_prompt(ending, Some("handoff")).unwrap();
        assert!(prompt.contains(OOM_REASON), "{prompt}");
        assert!(prompt.contains("shoal run"), "{prompt}");
        assert!(prompt.ends_with("\n\nhandoff"), "{prompt}");
        assert_eq!(prompt.contains("Continue"), turn == AgentState::Working);
    }
}
