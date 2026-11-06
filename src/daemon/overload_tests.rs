use super::notifications::NotificationKind;
use super::*;
use crate::daemon::{
    test_support::{
        install_test_script,
        overload::{
            bounded, child_command, fixture, launch, launch_child, report_session, run_child,
            serve_fixture, wait_launched, wait_paused, wait_started,
        },
    },
    workspace::StopRequest,
    workspace_files::Attacher,
};
use std::{fs, future::Future};

mod agent_restart;
mod idle_cleanup;
mod idle_stop;
mod queue_move;
mod restart;

#[tokio::test]
async fn built_in_agents_resume_without_configuration_or_a_session_picker() {
    let root = tempfile::tempdir().unwrap();
    let mut command = crate::daemon::test_support::isolated_test(
        root.path(),
        "daemon::overload_tests::built_in_restore_child",
    );
    assert!(bounded(command.status()).await.unwrap().success());
}

#[tokio::test]
#[ignore = "isolated recovery lookup launched by built-in restore test"]
async fn built_in_restore_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    use crate::execution::recovery::Recovery;
    let (_root, manager, workspace, serving) = fixture().await;
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[agents.codex]\ncommand = ['codex-fixture', '{args}']\n[agents.claude]\ncommand = ['claude-fixture', '{args}']\n",
    )
    .unwrap();
    let codex = [
        "codex-fixture",
        "resume",
        "--last",
        "-c",
        "features.worktrees=false",
    ];
    for (agent, handoff, restore) in [
        ("codex", None, &codex[..]),
        ("claude", None, &["claude-fixture", "--continue"][..]),
        ("codex", Some("handoff"), &codex[..]),
        (
            "claude",
            Some("handoff"),
            &["claude-fixture", "--continue"][..],
        ),
    ] {
        let recovery = Recovery::resolve(
            &manager.paths,
            &workspace,
            restored(agent, None),
            handoff,
            None,
        )
        .await
        .unwrap();
        assert!(recovery.automatic);
        assert_eq!(recovery.handoff_delivered, handoff.is_some());
        // Restored sessions keep receiving agent messages and may work in
        // `/tmp`, the default agent directory, and the child workspaces
        // beside theirs.
        let directory = workspace.path.parent().unwrap();
        let tools = crate::ai::tools(&Default::default());
        let tool = &tools[agent];
        let directories = [fs::canonicalize("/tmp").unwrap(), directory.to_path_buf()];
        let launch_args =
            crate::cli::agents::hook_args(tool.integration, (&workspace).into(), false)
                .unwrap()
                .into_iter()
                .chain(
                    directories
                        .iter()
                        .flat_map(|directory| tool.launch.arguments.add_dir_args(directory)),
                );
        // The restored session is told its mode's rules again, after its
        // resume arguments.
        let target = crate::protocol::ConfigTarget::Workspace(workspace.id.clone());
        let settings = crate::client::workspaces::settings(&manager.paths, target)
            .await
            .unwrap();
        let rules =
            crate::cli::agents::restored_instruction_args(&settings, agent, &workspace, "worker");
        assert!(!rules.is_empty());
        let expected: Vec<std::ffi::OsString> = restore
            .iter()
            .map(std::ffi::OsString::from)
            .chain(rules)
            .chain(launch_args)
            .collect();
        let (prompt, arguments) = recovery.command.split_last().unwrap();
        assert_eq!(arguments, expected);
        if let Some(handoff) = handoff {
            assert!(
                prompt
                    .to_string_lossy()
                    .ends_with(&format!("\n\n{handoff}")),
                "{prompt:?}"
            );
        }
    }
    // A reported session is resumed by its ID.
    for (agent, restore) in [
        ("codex", &["codex-fixture", "resume", "s1"][..]),
        ("claude", &["claude-fixture", "--resume", "s1"][..]),
    ] {
        let recovery = Recovery::resolve(
            &manager.paths,
            &workspace,
            restored(agent, Some("s1")),
            None,
            None,
        )
        .await
        .unwrap();
        let restore = restore.iter().map(std::ffi::OsString::from);
        assert!(recovery.command.starts_with(&restore.collect::<Vec<_>>()));
    }
    // An agent restored in a mode of its repository's own is told that
    // mode's rules again, but not the instructions its first message had.
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[agents.codex]\ncommand = ['codex-fixture', '{args}']\n\
         [agents.claude]\ncommand = ['claude-fixture', '{args}']\n\
         [modes.triage]\nrules = ['triage']\ninstructions = 'Label it.'\n\
         [rules.triage]\ntext = 'Triage {branch}'\n",
    )
    .unwrap();
    let triage = crate::execution::recovery::Restored {
        mode: "triage",
        ..restored("claude", Some("s1"))
    };
    let recovery = Recovery::resolve(&manager.paths, &workspace, triage, None, None)
        .await
        .unwrap();
    let told =
        |text: &str| (recovery.command.iter()).any(|arg| arg.to_string_lossy().contains(text));
    assert!(told("\n\nTriage load"), "{:?}", recovery.command);
    assert!(!told("Label it."), "{:?}", recovery.command);
    // A restore keeps the model and effort the agent ran with.
    let model = crate::agent::ModelChoice {
        model: Some("m1".into()),
        effort: Some("high".into()),
    };
    for (agent, restore) in [
        (
            "codex",
            &[
                "codex-fixture",
                "resume",
                "s1",
                "-m",
                "m1",
                "-c",
                "model_reasoning_effort=\"high\"",
            ][..],
        ),
        (
            "claude",
            &[
                "claude-fixture",
                "--resume",
                "s1",
                "--model",
                "m1",
                "--effort",
                "high",
            ][..],
        ),
    ] {
        let restored = crate::execution::recovery::Restored {
            mode: "worker",
            agent,
            model: &model,
            session: Some("s1"),
        };
        let recovery = Recovery::resolve(&manager.paths, &workspace, restored, None, None)
            .await
            .unwrap();
        let restore = restore.iter().map(std::ffi::OsString::from);
        assert!(
            recovery.command.starts_with(&restore.collect::<Vec<_>>()),
            "{:?}",
            recovery.command
        );
        assert_eq!(recovery.model, model);
    }
    let unsupported = Recovery::resolve(
        &manager.paths,
        &workspace,
        restored("custom", None),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(!unsupported.automatic);
    assert!(unsupported.command.is_empty());
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[agents.codex]\nresume_command = ['saved-session', 'specific-id']\n",
    )
    .unwrap();
    let overridden = Recovery::resolve(
        &manager.paths,
        &workspace,
        restored("codex", None),
        Some("handoff"),
        None,
    )
    .await
    .unwrap();
    assert!(overridden.automatic);
    assert!(!overridden.handoff_delivered);
    assert_eq!(
        overridden.command,
        ["saved-session", "specific-id"].map(std::ffi::OsString::from)
    );
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[agents.codex]\nresume_command = ['saved-session', '--', '{prompt}']\n",
    )
    .unwrap();
    let prompted = Recovery::resolve(
        &manager.paths,
        &workspace,
        restored("codex", None),
        Some("handoff"),
        None,
    )
    .await
    .unwrap();
    assert!(prompted.handoff_delivered);
    let [program, separator, prompt] = &prompted.command[..] else {
        panic!("{:?}", prompted.command);
    };
    assert_eq!([program, separator], ["saved-session", "--"]);
    assert!(
        prompt.to_string_lossy().ends_with("\n\nhandoff"),
        "{prompt:?}"
    );
    let prompted = Recovery::resolve(
        &manager.paths,
        &workspace,
        restored("codex", None),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(!prompted.handoff_delivered);
    let [program, separator, prompt] = &prompted.command[..] else {
        panic!("{:?}", prompted.command);
    };
    assert_eq!([program, separator], ["saved-session", "--"]);
    assert!(!prompt.to_string_lossy().contains("handoff"), "{prompt:?}");
    // A configured resume command gets the model ahead of its prompt.
    let model = crate::agent::ModelChoice {
        model: Some("m1".into()),
        effort: None,
    };
    let with_model = |agent| crate::execution::recovery::Restored {
        mode: "worker",
        agent,
        model: &model,
        session: None,
    };
    let kept = Recovery::resolve(&manager.paths, &workspace, with_model("codex"), None, None)
        .await
        .unwrap();
    assert_eq!(
        kept.command[..4],
        ["saved-session", "-m", "m1", "--"].map(std::ffi::OsString::from)
    );
    // A tool that cannot take it refuses the restore, which waits.
    let refused = Recovery::resolve(&manager.paths, &workspace, with_model("custom"), None, None);
    assert!(refused.await.is_err());
    // So does the restore of an agent config turns off.
    let config = fs::read_to_string(workspace.path.join(".shoal.toml")).unwrap();
    fs::write(
        workspace.path.join(".shoal.toml"),
        format!("{config}enabled = false\n"),
    )
    .unwrap();
    let off = Recovery::resolve(&manager.paths, &workspace, with_model("codex"), None, None);
    assert!(off.await.is_err());
    fs::write(workspace.path.join(".shoal.toml"), config).unwrap();
    // A restore tells the session why and when it stopped.
    let stop = crate::execution::recovery::Stop {
        reason: Some("critical memory pressure".into()),
        at: None,
    };
    let told = Recovery::resolve(
        &manager.paths,
        &workspace,
        restored("codex", None),
        None,
        Some(&stop),
    )
    .await
    .unwrap();
    let [_, _, prompt] = &told.command[..] else {
        panic!("{:?}", told.command);
    };
    assert!(
        prompt
            .to_string_lossy()
            .contains("(critical memory pressure)"),
        "{prompt:?}"
    );
    serving.abort();
}

/// Waits until the stopped agent's restore waits in the agent queue.
#[track_caller]
fn wait_restore_queued(manager: &Manager) -> impl Future<Output = ()> + '_ {
    bounded(async move {
        // Subscribe before the first check so a concurrent commit cannot be missed.
        let mut changed = manager.store.watch_changes();
        while !manager
            .queue_entries()
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.kind == crate::state::QueueKind::Restore)
        {
            changed.changed().await.unwrap();
        }
    })
}

async fn queue_is_empty(manager: &Manager) -> bool {
    manager.queue_entries().await.unwrap().is_empty()
}

#[tokio::test]
#[ignore = "isolated tracked wrapper launched by overload tests"]
async fn tracked_agent_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    run_tracked_agent("trap 'exit 0' TERM; touch started; while :; do sleep 1; done").await;
}

/// An agent that keeps running when stopped. Once the stop reaches it, it
/// suspends its parent, the wrapper, and writes `terminated`, so the wrapper
/// stays where the stop reached the agent.
#[tokio::test]
#[ignore = "isolated tracked wrapper launched by overload tests"]
async fn tracked_agent_holding_on() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    run_tracked_agent(
        "trap 'kill -STOP $PPID; touch terminated' TERM; touch started; while :; do sleep 1; done",
    )
    .await;
}

/// Like `tracked_agent_child`, with a background command in its own process
/// group, as Claude Code runs shell commands. It writes its PID to `background`
/// and keeps running in Python: macOS hides platform binaries' environments.
#[tokio::test]
#[ignore = "isolated tracked wrapper launched by overload tests"]
async fn tracked_agent_with_background_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    run_tracked_agent(&with_background("touch started; while :; do sleep 1; done")).await;
}

/// Like `tracked_agent_with_background_child`, but the agent exits on its own
/// once the test creates `exit`, leaving its background command running.
#[tokio::test]
#[ignore = "isolated tracked wrapper launched by overload tests"]
async fn tracked_agent_exiting_with_background_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    run_tracked_agent(&with_background(
        "touch started; while [ ! -e exit ]; do sleep 1; done",
    ))
    .await;
}

/// An agent that exits on its own once the test creates `exit`.
#[tokio::test]
#[ignore = "isolated tracked wrapper launched by overload tests"]
async fn tracked_agent_exiting_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    run_tracked_agent("trap 'exit 0' TERM; touch started; while [ ! -e exit ]; do sleep 1; done")
        .await;
}

/// Run `script` after starting a background command in its own process group.
fn with_background(script: &str) -> String {
    concat!(
        "trap 'exit 0' TERM; python3 -c \"",
        "import os, time; os.setpgid(0, 0); ",
        "open('background.tmp', 'w').write(str(os.getpid())); ",
        "os.rename('background.tmp', 'background'); ",
        "time.sleep(300)\" & ",
    )
    .to_owned()
        + script
}

async fn run_tracked_agent(script: &str) {
    let root = std::env::current_dir().unwrap();
    let paths = crate::paths::Paths::for_test(root);
    let id = std::env::var("SHOAL_TEST_WORKSPACE").unwrap();
    let code = crate::execution::run(
        &paths,
        id,
        vec!["sh".into(), "-c".into(), script.into()],
        Some(crate::execution::TrackedAgent::named("fixture")),
    )
    .await
    // A daemon crash ends the wrapper with an error, as it does a real one.
    .unwrap_or_else(|error| {
        eprintln!("shoal: {error:#}");
        1
    });
    std::process::exit(code);
}

#[tokio::test]
#[ignore = "isolated tracked command launched by the stop test"]
async fn tracked_command_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    let root = std::env::current_dir().unwrap();
    let paths = crate::paths::Paths::for_test(root);
    let id = std::env::var("SHOAL_TEST_WORKSPACE").unwrap();
    let code = crate::execution::run_command(
        &paths,
        id,
        vec![
            "sh".into(),
            "-c".into(),
            "trap 'exit 0' TERM; touch command-started; while :; do sleep 1; done".into(),
        ],
    )
    .await
    .unwrap();
    std::process::exit(code);
}

#[tokio::test]
async fn an_agent_whose_terminal_closes_is_saved_as_stopped() {
    use crate::execution::recovery::{LastSession, Record, Saved};
    let (_root, manager, workspace, serving) = fixture().await;
    let agent = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    report_session(&manager, &workspace, "closed-session").await;
    let wrapper = manager
        .inspect_workspace(&workspace.id)
        .await
        .unwrap()
        .executions[0]
        .wrapper
        .clone()
        .unwrap();
    crate::process::identity::signal(&wrapper, libc::SIGHUP).unwrap();
    assert_eq!(bounded(agent).await.unwrap().unwrap(), 129);
    assert_eq!(LastSession::all(&manager.paths, &workspace.id).unwrap(), []);
    let saved = Saved::load(&manager.paths, &workspace.id).unwrap();
    let record = Record::load(&saved.agents[0].1).unwrap().unwrap();
    assert_eq!(record.stop_reason.as_deref(), Some("its terminal closed"));
    assert_eq!(record.session.as_deref(), Some("closed-session"));
    serving.abort();
}

/// A shutdown terminates the wrapper and may kill it before its agent exits,
/// so the record is saved before the agent is stopped.
#[tokio::test]
async fn an_agent_whose_session_ends_is_saved_before_it_stops() {
    use crate::execution::recovery::{Record, record_path, restore_requests};
    let (_root, manager, workspace, serving) = fixture().await;
    let agent = launch_child(
        &manager,
        &workspace,
        "daemon::overload_tests::tracked_agent_holding_on",
    );
    wait_started(&manager, &workspace).await;
    report_session(&manager, &workspace, "ending-session").await;
    let execution = manager
        .inspect_workspace(&workspace.id)
        .await
        .unwrap()
        .executions
        .remove(0);
    // The daemon keeps the agent's name, which its session needs without a record.
    let id = execution.id.clone();
    let agent_name = manager
        .store
        .run(move |db| store::execution_agent(db, &id))
        .await
        .unwrap();
    assert_eq!(agent_name.as_deref(), Some("fixture"));
    let wrapper = execution.wrapper.clone().unwrap();
    crate::process::identity::signal(&wrapper, libc::SIGTERM).unwrap();
    // The agent traps the stop the wrapper sends it, which follows the save,
    // and suspends the wrapper before it could save once the agent stopped.
    bounded(async {
        while !workspace.path.join("terminated").is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    crate::process::identity::signal(&wrapper, libc::SIGKILL).unwrap();
    assert_eq!(bounded(agent).await.unwrap().unwrap(), 128 + libc::SIGKILL);
    // SAFETY: the killed wrapper's agent group, which nothing else stops.
    unsafe {
        libc::kill(-(execution.group_id.unwrap() as i32), libc::SIGKILL);
    }
    let path = record_path(&manager.paths, &workspace.id, &execution.id);
    let record = Record::load(&path).unwrap().unwrap();
    assert_eq!(record.agent, "fixture");
    assert_eq!(
        record.stop_reason.as_deref(),
        Some("its session was terminated")
    );
    assert_eq!(record.session.as_deref(), Some("ending-session"));
    // It comes back through the agent queue once its execution has ended.
    let requested: Vec<_> = restore_requests(&manager.paths, &workspace.id)
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(requested, [execution.id]);
    serving.abort();
}

#[tokio::test]
async fn overload_restores_the_configured_session_and_keeps_waiting_execution_owned() {
    let (_root, manager, workspace, serving) = fixture().await;
    fs::write(
        workspace.path.join("resume.sh"),
        "#!/bin/sh\nprintf restored > restored\nexit 7\n",
    )
    .unwrap();
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let monitor = manager.gates.operation().await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    drop(monitor);
    // An update waits for the restore, so the stop holds one back.
    assert!(manager.gates.try_quiesce().is_none());
    wait_paused(&manager, &workspace).await;
    wait_restore_queued(&manager).await;
    assert!(manager.gates.try_quiesce().is_none());
    assert!(!launched.is_finished());
    assert!(!workspace.path.join("restored").exists());
    let epoch = manager.overload_recovery.epoch();
    manager.overload_recovery.publish(Some(epoch));
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 7);
    // The restored agent took over the slot its restore kept.
    assert!(queue_is_empty(&manager).await);
    drop(bounded(manager.gates.quiesce()).await);
    assert!(!crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    assert!(
        manager
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .executions
            .is_empty()
    );
    assert!(
        manager
            .notifications(false, 10)
            .await
            .unwrap()
            .iter()
            .any(|event| event.kind == NotificationKind::AgentResumed)
    );
    serving.abort();
}

/// A wrapper that cannot start the agent it restores, as one whose binary an
/// upgrade removed cannot, leaves the restore to the agent queue instead of
/// keeping the agent as one that exited on its own.
#[tokio::test]
async fn a_restore_its_wrapper_fails_to_start_waits_in_the_agent_queue() {
    use crate::execution::recovery::{LastSession, record_path};
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let execution = manager
        .inspect_workspace(&workspace.id)
        .await
        .unwrap()
        .executions
        .remove(0)
        .id;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    wait_restore_queued(&manager).await;
    fs::remove_file(workspace.path.join("resume.sh")).unwrap();
    let epoch = manager.overload_recovery.epoch();
    manager.overload_recovery.publish(Some(epoch));
    assert_ne!(bounded(launched).await.unwrap().unwrap(), 0);
    let entries = manager.queue_entries().await.unwrap();
    let restore = entries
        .iter()
        .find(|entry| entry.execution.as_deref() == Some(execution.as_str()));
    assert_eq!(
        restore.map(|entry| entry.kind),
        Some(crate::state::QueueKind::Restore)
    );
    assert!(record_path(&manager.paths, &workspace.id, &execution).is_file());
    assert_eq!(LastSession::all(&manager.paths, &workspace.id).unwrap(), []);
    let messages: Vec<_> = manager
        .notifications(false, 10)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == NotificationKind::AgentExited)
        .map(|event| event.message)
        .collect();
    assert_eq!(messages.len(), 1, "{messages:?}");
    serving.abort();
}

#[tokio::test]
async fn an_idle_agent_waiting_for_an_update_lets_a_daemon_update_through() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    let id = manager
        .inspect_workspace(&workspace.id)
        .await
        .unwrap()
        .executions[0]
        .id
        .clone();
    manager
        .set_agent_state(
            &workspace.name,
            crate::state::AgentState::Idle,
            Some(id.clone()),
            None,
        )
        .await
        .unwrap();
    let monitor = manager.gates.operation().await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    drop(monitor);
    wait_paused(&manager, &workspace).await;
    wait_restore_queued(&manager).await;
    // Waiting for an update lets go of the hold once its processes stopped.
    bounded(async {
        while manager.gates.try_quiesce().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(!launched.is_finished());
    // The fixture runs no disk monitor; a reading finds the space free.
    let generation = manager.config_state.generation();
    manager.config_state.publish_disk_reading(Some(generation));
    manager
        .send_agent_message(
            &workspace.name,
            "Review requested".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 0);
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    assert!(
        manager
            .notifications(false, 10)
            .await
            .unwrap()
            .iter()
            .any(|event| event.message.ends_with("for an update"))
    );
    serving.abort();
}

/// Launch an agent with a background command outside its process group and
/// return that command's identity once both run.
async fn launch_with_background(
    manager: &Manager,
    workspace: &crate::model::Workspace,
) -> (
    tokio::task::JoinHandle<Result<i32>>,
    crate::process::identity::Identity,
) {
    launch_agent_with_background(
        manager,
        workspace,
        "daemon::overload_tests::tracked_agent_with_background_child",
    )
    .await
}

/// Launch the agent helper `test`, which starts a background command outside
/// its process group, and return that command's identity once both run.
async fn launch_agent_with_background(
    manager: &Manager,
    workspace: &crate::model::Workspace,
    test: &str,
) -> (
    tokio::task::JoinHandle<Result<i32>>,
    crate::process::identity::Identity,
) {
    let launched = launch_child(manager, workspace, test);
    wait_launched(manager, workspace, "background", 1).await;
    wait_started(manager, workspace).await;
    let pid = fs::read_to_string(workspace.path.join("background")).unwrap();
    let background = crate::process::identity::capture(pid.parse().unwrap())
        .unwrap()
        .unwrap();
    (launched, background)
}

#[tokio::test]
async fn agent_exit_stops_commands_outside_the_agent_group() {
    let (_root, manager, workspace, serving) = fixture().await;
    let (launched, background) = launch_agent_with_background(
        &manager,
        &workspace,
        "daemon::overload_tests::tracked_agent_exiting_with_background_child",
    )
    .await;
    fs::write(workspace.path.join("exit"), "").unwrap();
    // The wrapper exits only after the daemon acknowledges the exit.
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 0);
    assert!(!crate::process::identity::alive(&background).unwrap());
    assert!(
        manager
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .executions
            .is_empty()
    );
    serving.abort();
}

#[tokio::test]
async fn overload_stops_commands_outside_the_agent_group_and_restores_it() {
    let (_root, manager, workspace, serving) = fixture().await;
    let (launched, background) = launch_with_background(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    assert!(!crate::process::identity::alive(&background).unwrap());
    publish_recovery(&manager);
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 0);
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    serving.abort();
}

#[tokio::test]
async fn overload_without_recovery_stops_commands_outside_the_agent_group() {
    let (_root, manager, workspace, serving) = fixture().await;
    set_recovery(&manager, false).await;
    let (launched, background) = launch_with_background(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    bounded(launched).await.unwrap().unwrap();
    assert!(!crate::process::identity::alive(&background).unwrap());
    // The execution ended proven, so `shoal resume` can select its record.
    assert!(
        manager
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .executions
            .is_empty()
    );
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}

#[tokio::test]
async fn cancelling_a_restore_still_stops_commands_outside_the_agent_group() {
    let (_root, manager, workspace, serving) = fixture().await;
    let (launched, background) = launch_with_background(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    // Disabling recovery cancels the restore however far the pause has come.
    set_recovery(&manager, false).await;
    bounded(launched).await.unwrap().unwrap();
    assert!(!crate::process::identity::alive(&background).unwrap());
    assert!(
        manager
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .executions
            .is_empty()
    );
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}

/// Set the recorded process group of the workspace's only execution.
async fn record_group(manager: &Manager, workspace: &crate::model::Workspace, group: Option<u32>) {
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE executions SET group_id=?2 WHERE workspace_id=?1",
                rusqlite::params![id, group],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn unproven_stop_keeps_the_agent_waiting_until_proof_then_restores_it() {
    let (_root, manager, workspace, serving) = fixture().await;
    let mut command = child_command(
        &manager,
        &workspace,
        "daemon::overload_tests::tracked_agent_child",
    );
    let terminal = manager.paths.home.join("terminal");
    command.stderr(fs::File::create(&terminal).unwrap());
    let launched = run_child(command);
    wait_started(&manager, &workspace).await;
    // An incomplete launch identity stands in for survivors Shoal cannot stop.
    let group = manager
        .inspect_workspace(&workspace.id)
        .await
        .unwrap()
        .executions[0]
        .group_id;
    record_group(&manager, &workspace, None).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    bounded(async {
        while !manager
            .notifications(false, 10)
            .await
            .unwrap()
            .iter()
            .any(|event| event.kind == NotificationKind::RecoveryBlocked)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    publish_recovery(&manager);
    assert!(!launched.is_finished());
    assert!(!workspace.path.join("restored").exists());
    record_group(&manager, &workspace, group).await;
    assert_eq!(bounded(launched).await.unwrap().unwrap(), 0);
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    // The agent's terminal says what the restore waits for, as the user's notification does.
    let terminal = fs::read_to_string(&terminal).unwrap();
    assert!(
        terminal.contains("its launch identity is incomplete"),
        "{terminal}"
    );
    serving.abort();
}

#[tokio::test]
async fn manual_stop_cancels_waiting_recovery_and_leaves_a_restore_record() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    wait_restore_queued(&manager).await;
    bounded(manager.stop_workspace(&workspace.id, StopRequest::manual()))
        .await
        .unwrap();
    bounded(launched).await.unwrap().unwrap();
    assert!(!workspace.path.join("restored").exists());
    assert!(queue_is_empty(&manager).await);
    let records = fs::read_dir(manager.paths.workspace_state(&workspace.id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with(".recovery.json"))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1);
    let text = fs::read_to_string(&records[0]).unwrap();
    let mut record: serde_json::Value = serde_json::from_str(&text).unwrap();
    // The wrapper that waited has exited, so resume restores the agent.
    let waiting = record.as_object_mut().unwrap().remove("waiting").unwrap();
    let waiting: crate::process::identity::Identity = serde_json::from_value(waiting).unwrap();
    assert!(!crate::process::identity::alive(&waiting).unwrap());
    assert!(crate::execution::recovery::any_pending(&manager.paths));
    assert_eq!(
        record,
        serde_json::json!({
            "agent": "fixture",
            "stop_reason": "test memory pressure",
            "stage": "stopped",
            "mode": "worker",
        })
    );
    assert!(
        manager
            .cleanup_snapshot(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    serving.abort();
}

#[tokio::test]
async fn failed_restore_expansion_does_not_block_launch_or_manual_recovery_record() {
    let (_root, manager, workspace, serving) = fixture().await;
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE workspaces SET base_ref='refs/heads/missing' WHERE id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[agents.fixture]\nresume_command = ['./resume.sh', '{diff_base}']\n",
    )
    .unwrap();
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    bounded(launched).await.unwrap().unwrap();
    assert!(!workspace.path.join("restored").exists());
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    assert!(
        manager
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .executions
            .is_empty()
    );
    serving.abort();
}

#[tokio::test]
async fn refused_recovery_finishes_the_stopped_execution_and_retains_manual_recovery() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    manager
        .set_state(&workspace.id, crate::state::WorkspaceState::Failed, None)
        .await
        .unwrap();
    let epoch = manager.overload_recovery.epoch();
    manager.overload_recovery.publish(Some(epoch));
    bounded(launched).await.unwrap().unwrap();
    assert!(
        manager
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .executions
            .is_empty()
    );
    assert!(!workspace.path.join("restored").exists());
    assert!(queue_is_empty(&manager).await);
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}

#[tokio::test]
async fn a_done_workspace_does_not_restore_its_stopped_agent() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    manager
        .mark_done(&workspace.id, false, false, vec![], &Attacher::User)
        .await
        .unwrap();
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    bounded(launched).await.unwrap().unwrap();
    assert!(!workspace.path.join("restored").exists());
    assert!(queue_is_empty(&manager).await);
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}

#[tokio::test]
async fn finishing_the_assignment_cancels_a_waiting_restore() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    wait_restore_queued(&manager).await;
    manager
        .mark_done(&workspace.id, false, false, vec![], &Attacher::User)
        .await
        .unwrap();
    publish_recovery(&manager);
    bounded(launched).await.unwrap().unwrap();
    assert!(!workspace.path.join("restored").exists());
    assert!(queue_is_empty(&manager).await);
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}

async fn set_recovery(manager: &Manager, enabled: bool) {
    crate::daemon::test_support::configure(
        manager,
        &format!("[machine.overload.recovery]\nenabled = {enabled}\n"),
    )
    .await;
}

fn publish_recovery(manager: &Manager) {
    let epoch = manager.overload_recovery.epoch();
    manager.overload_recovery.publish(Some(epoch));
}

#[tokio::test]
async fn disabling_recovery_while_an_agent_waits_finishes_it_with_a_restore_record() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    assert!(manager.stop_agent_for_overload("test pressure").await);
    wait_paused(&manager, &workspace).await;
    // Load never recovers here: disabling alone must end the wait.
    set_recovery(&manager, false).await;
    bounded(launched).await.unwrap().unwrap();
    assert!(!workspace.path.join("restored").exists());
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}

#[tokio::test]
async fn enabling_recovery_applies_to_an_agent_started_without_it() {
    let (_root, manager, workspace, serving) = fixture().await;
    set_recovery(&manager, false).await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    set_recovery(&manager, true).await;
    assert!(manager.stop_agent_for_overload("test pressure").await);
    wait_paused(&manager, &workspace).await;
    publish_recovery(&manager);
    bounded(launched).await.unwrap().unwrap();
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    serving.abort();
}

#[tokio::test]
async fn recovery_disconnect_closes_the_connection_without_forgetting_ownership() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    wait_restore_queued(&manager).await;
    launched.abort();
    bounded(async {
        loop {
            let inspection = manager.inspect_workspace(&workspace.id).await.unwrap();
            if inspection
                .executions
                .iter()
                .any(|execution| execution.state == crate::state::ExecutionState::Unknown)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(!workspace.path.join("restored").exists());
    // The restore keeps its place while the wrapper may reattach.
    assert!(!queue_is_empty(&manager).await);
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    assert!(
        !reopened
            .stop_agent_for_overload("test overload after restart")
            .await
    );
    assert!(
        !reopened
            .inspect_workspace(&workspace.id)
            .await
            .unwrap()
            .executions
            .is_empty()
    );
    reopened.store.shutdown().await;
    serving.abort();
}

#[tokio::test]
async fn critical_disk_space_stops_commands_and_restores_agents_once_space_recovers() {
    let (_root, manager, workspace, serving) = fixture().await;
    let agent = launch(&manager, &workspace);
    let command = launch_child(
        &manager,
        &workspace,
        "daemon::overload_tests::tracked_command_child",
    );
    wait_started(&manager, &workspace).await;
    wait_launched(&manager, &workspace, "command-started", 2).await;
    let mut monitor = super::disk::Monitor::default();
    let now = tokio::time::Instant::now();
    let empty = |_: &std::path::Path| Ok(0);
    bounded(monitor.check(&manager, now, empty)).await.unwrap();
    bounded(command).await.unwrap().unwrap();
    wait_paused(&manager, &workspace).await;
    let records = fs::read_dir(manager.paths.workspace_state(&workspace.id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with(".recovery.json"))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1);
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(&records[0]).unwrap()).unwrap();
    let reason = record["stop_reason"].as_str().unwrap();
    assert!(reason.contains("0.0 GiB free"), "{reason}");
    let messages = manager
        .notifications(false, 10)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == NotificationKind::AgentStopped)
        .map(|event| event.message)
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 2, "{messages:?}");
    // Later critical readings leave the waiting agent's restore in place.
    bounded(monitor.check(&manager, now, empty)).await.unwrap();
    assert!(!manager.config_state.disk_space_recovered());
    bounded(monitor.check(&manager, now, |_: &std::path::Path| Ok(u64::MAX)))
        .await
        .unwrap();
    assert!(manager.config_state.disk_space_recovered());
    publish_recovery(&manager);
    assert_eq!(bounded(agent).await.unwrap().unwrap(), 0);
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    serving.abort();
}

/// The restore of `agent` with the tool's own model and effort, from
/// `session` when given.
fn restored<'a>(
    agent: &'a str,
    session: Option<&'a str>,
) -> crate::execution::recovery::Restored<'a> {
    static OWN: crate::agent::ModelChoice = crate::agent::ModelChoice {
        model: None,
        effort: None,
    };
    crate::execution::recovery::Restored {
        mode: "worker",
        agent,
        model: &OWN,
        session,
    }
}
