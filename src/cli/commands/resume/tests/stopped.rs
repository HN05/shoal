//! Resuming stopped work through `shoal resume`, against the overload
//! tests' fixture daemon.
use crate::client::assignments::Done;
use crate::client::space_ids::SpaceId;
use crate::execution::recovery::LastSessionExt;
use std::{fs, os::unix::fs::PermissionsExt};

use anyhow::Result;

use crate::client::space_names::SpaceSelector;
use crate::{
    client::executions::ExecutionKind,
    daemon::{
        test_support::{
            isolated_test,
            overload::{
                bounded, fixture, launch, launch_child, report_session, wait_launched, wait_paused,
                wait_started,
            },
        },
        workspace::{Manager, StopRequest},
        workspace_files::Attacher,
    },
    protocol::RemovalMethod,
};

// These cases call CLI handlers in-process. Run them in an env-cleared child
// so a tracked test runner cannot lend its scope to the fixture daemon.
async fn run_isolated_test(name: &str) {
    let root = tempfile::tempdir().unwrap();
    let mut command = isolated_test(
        root.path(),
        "cli::commands::resume::tests::stopped::isolated_overload_test",
    );
    command.env("SHOAL_OVERLOAD_TEST", name);
    assert!(bounded(command.status()).await.unwrap().success());
}

#[tokio::test]
#[ignore = "isolated overload test invoked by its in-process wrapper"]
async fn isolated_overload_test() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    match std::env::var("SHOAL_OVERLOAD_TEST").unwrap().as_str() {
        "stop_saves_agents_and_commands_preserving_work_and_leases" => {
            stop_saves_agents_and_commands_preserving_work_and_leases_body().await
        }
        "resume_without_an_agent_reports_stopped_commands_once" => {
            resume_without_an_agent_reports_stopped_commands_once_body().await
        }
        "a_restored_stopped_agent_is_told_the_commands_stopped_with_it" => {
            a_restored_stopped_agent_is_told_the_commands_stopped_with_it_body().await
        }
        "resume_continues_an_agent_that_exited_on_its_own" => {
            resume_continues_an_agent_that_exited_on_its_own_body().await
        }
        "discard_clears_pending_recovery_without_launching_an_agent" => {
            discard_clears_pending_recovery_without_launching_an_agent_body().await
        }
        "manual_resume_consumes_the_record_on_start_even_if_other_commands_run_or_restore_fails" => {
            manual_resume_consumes_the_record_on_start_even_if_other_commands_run_or_restore_fails_body().await
        }
        name => panic!("unknown isolated overload test: {name}"),
    }
}

#[tokio::test]
#[ignore = "isolated stop operation launched by overload tests"]
async fn stop_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    let root = std::env::var_os("SHOAL_TEST_ROOT").unwrap();
    let paths = crate::paths::Paths::for_test(root);
    let id = SpaceId::from(std::env::var("SHOAL_TEST_WORKSPACE").unwrap());
    crate::client::connection::request::<crate::model::StoppedWorkspace>(
        &paths,
        RemovalMethod::StopWorkspace {
            workspace: SpaceSelector::id(&id),
            acknowledge_unproven: false,
            launches: crate::protocol::QueuedLaunches::Cancel,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "isolated resume operation launched by overload tests"]
async fn resume_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    let root = std::env::var_os("SHOAL_TEST_ROOT").unwrap();
    let paths = crate::paths::Paths::for_test(root);
    let id = SpaceId::from(std::env::var("SHOAL_TEST_WORKSPACE").unwrap());
    let discard = std::env::var("SHOAL_TEST_DISCARD").unwrap() == "true";
    let ctx = crate::cli::context::Context::new(paths, true);
    match crate::cli::commands::resume::run(&ctx, SpaceSelector::id(&id), None, discard, true, None)
        .await
    {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
    }
}

fn isolated_operation(
    manager: &Manager,
    workspace: &crate::model::Workspace,
    test: &str,
) -> tokio::process::Command {
    let mut command = isolated_test(&manager.paths.home, test);
    command
        .env("SHOAL_TEST_ROOT", &manager.paths.home)
        .env("SHOAL_TEST_WORKSPACE", &workspace.id);
    command
}

fn resume_isolated(
    manager: &Manager,
    workspace: &crate::model::Workspace,
    discard: bool,
) -> tokio::task::JoinHandle<Result<i32>> {
    let mut command = isolated_operation(
        manager,
        workspace,
        "cli::commands::resume::tests::stopped::resume_child",
    );
    command.env("SHOAL_TEST_DISCARD", discard.to_string());
    tokio::spawn(async move {
        let status = command.status().await?;
        Ok(crate::execution::exit_code(status))
    })
}

async fn stop_isolated(manager: &Manager, workspace: &crate::model::Workspace) -> Result<i32> {
    let mut command = isolated_operation(
        manager,
        workspace,
        "cli::commands::resume::tests::stopped::stop_child",
    );
    let status = command.status().await?;
    Ok(crate::execution::exit_code(status))
}

#[tokio::test]
async fn stop_saves_agents_and_commands_preserving_work_and_leases() {
    run_isolated_test("stop_saves_agents_and_commands_preserving_work_and_leases").await;
}

async fn stop_saves_agents_and_commands_preserving_work_and_leases_body() {
    let (_root, manager, workspace, serving) = fixture().await;
    manager
        .acquire_port(
            &SpaceSelector::id(&workspace.id),
            "web".into(),
            crate::model::PortRequest::default(),
            None,
        )
        .await
        .unwrap();
    fs::write(workspace.path.join("unfinished"), "work in progress").unwrap();
    fs::remove_file(workspace.path.join(".shoal.toml")).unwrap();
    let agent = launch(&manager, &workspace);
    let command = launch_child(
        &manager,
        &workspace,
        "daemon::overload_tests::tracked_command_child",
    );
    wait_started(&manager, &workspace).await;
    wait_launched(&manager, &workspace, "command-started", 2).await;
    assert_eq!(
        bounded(stop_isolated(&manager, &workspace)).await.unwrap(),
        0
    );
    bounded(agent).await.unwrap().unwrap();
    bounded(command).await.unwrap().unwrap();
    let retained = manager
        .inspect_workspace(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(retained.workspace.state, workspace.state);
    assert_eq!(retained.ports.len(), 1);
    assert!(retained.executions.is_empty());
    assert_eq!(
        fs::read_to_string(workspace.path.join("unfinished")).unwrap(),
        "work in progress"
    );
    let commands = fs::read_dir(manager.paths.workspace_state(&workspace.id))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with(".command.json"))
        .map(|path| serde_json::from_slice(&fs::read(path).unwrap()).unwrap())
        .collect::<Vec<serde_json::Value>>();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0]["argv"][0], "sh");
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    // A manual stop never restores automatically.
    manager.overload_recovery.publish(Some(0));
    assert!(!workspace.path.join("restored").exists());
    fs::write(
        workspace.path.join("resume.sh"),
        "#!/bin/sh\nprintf '%s' \"$1\" > restored\n",
    )
    .unwrap();
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[agents.fixture]\nresume_command = ['./resume.sh', '{prompt}']\n",
    )
    .unwrap();
    assert_eq!(
        bounded(resume_isolated(&manager, &workspace, false))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    let handoff = fs::read_to_string(workspace.path.join("restored")).unwrap();
    assert!(handoff.contains("\n- sh -c 'trap"), "{handoff}");
    assert!(!crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}

#[tokio::test]
async fn a_restored_stopped_agent_is_told_the_commands_stopped_with_it() {
    run_isolated_test("a_restored_stopped_agent_is_told_the_commands_stopped_with_it").await;
}

async fn a_restored_stopped_agent_is_told_the_commands_stopped_with_it_body() {
    let (_root, manager, workspace, serving) = fixture().await;
    let agent = launch(&manager, &workspace);
    let command = launch_child(
        &manager,
        &workspace,
        "daemon::overload_tests::tracked_command_child",
    );
    wait_started(&manager, &workspace).await;
    wait_launched(&manager, &workspace, "command-started", 2).await;
    // A stop other than `shoal stop`, such as a daemon shutdown's, keeps the
    // agent as stopped work.
    bounded(manager.stop_workspace(&SpaceSelector::id(&workspace.id), StopRequest::manual()))
        .await
        .unwrap();
    bounded(agent).await.unwrap().unwrap();
    bounded(command).await.unwrap().unwrap();
    let saved = crate::execution::recovery::Saved::load(&manager.paths, &workspace.id).unwrap();
    assert_eq!((saved.agents.len(), saved.commands.len()), (1, 1));
    fs::write(
        workspace.path.join("resume.sh"),
        "#!/bin/sh\nprintf '%s' \"$1\" > restored\n",
    )
    .unwrap();
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[agents.fixture]\nresume_command = ['./resume.sh', '{prompt}']\n",
    )
    .unwrap();
    assert_eq!(
        bounded(resume_isolated(&manager, &workspace, false))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    let handoff = fs::read_to_string(workspace.path.join("restored")).unwrap();
    assert!(handoff.contains("\n- sh -c 'trap"), "{handoff}");
    assert!(!crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    serving.abort();
}

#[tokio::test]
async fn resume_without_an_agent_reports_stopped_commands_once() {
    run_isolated_test("resume_without_an_agent_reports_stopped_commands_once").await;
}

async fn resume_without_an_agent_reports_stopped_commands_once_body() {
    let (_root, manager, workspace, serving) = fixture().await;
    let command = launch_child(
        &manager,
        &workspace,
        "daemon::overload_tests::tracked_command_child",
    );
    wait_launched(&manager, &workspace, "command-started", 1).await;
    bounded(manager.stop_workspace(&SpaceSelector::id(&workspace.id), StopRequest::manual()))
        .await
        .unwrap();
    bounded(command).await.unwrap().unwrap();
    assert!(crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    let resume = || resume_isolated(&manager, &workspace, false);
    assert_eq!(bounded(resume()).await.unwrap().unwrap(), 0);
    assert!(!crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    assert_ne!(bounded(resume()).await.unwrap().unwrap(), 0);
    serving.abort();
}

#[tokio::test]
async fn resume_continues_an_agent_that_exited_on_its_own() {
    run_isolated_test("resume_continues_an_agent_that_exited_on_its_own").await;
}

async fn resume_continues_an_agent_that_exited_on_its_own_body() {
    use crate::execution::recovery::{LastSession, Saved};
    let (_root, manager, workspace, serving) = fixture().await;
    let last = || LastSession::all(&manager.paths, &workspace.id).unwrap();
    // An agent `shoal stop` stops leaves its session as one that exits on its
    // own does, which uses no slot and keeps nothing from cleanup.
    let agent = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    report_session(&manager, &workspace, "stopped-session").await;
    assert_eq!(
        bounded(stop_isolated(&manager, &workspace)).await.unwrap(),
        0
    );
    bounded(agent).await.unwrap().unwrap();
    let [stopped] = last().try_into().unwrap();
    assert_eq!(stopped.session.as_deref(), Some("stopped-session"));
    assert!(
        Saved::load(&manager.paths, &workspace.id)
            .unwrap()
            .agents
            .is_empty()
    );
    assert!(!crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());

    // A new agent replaces it, and leaves its own session when it exits on
    // its own.
    fs::remove_file(workspace.path.join("started")).unwrap();
    let agent = launch_child(
        &manager,
        &workspace,
        "daemon::overload_tests::tracked_agent_exiting_child",
    );
    wait_started(&manager, &workspace).await;
    report_session(&manager, &workspace, "quit-session").await;
    fs::write(workspace.path.join("exit"), "").unwrap();
    assert_eq!(bounded(agent).await.unwrap().unwrap(), 0);
    let [quit] = last().try_into().unwrap();
    assert_eq!(
        (quit.agent.name(), quit.session.as_deref(), quit.moved),
        ("fixture", Some("quit-session"), false)
    );
    assert!(!crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    assert_eq!(
        bounded(resume_isolated(&manager, &workspace, false))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "restored"
    );
    // The restored agent exited on its own too; finishing the assignment
    // forgets it.
    assert!(!last().is_empty());
    manager
        .mark_done(
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(last(), []);
    assert_ne!(
        bounded(resume_isolated(&manager, &workspace, false))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    serving.abort();
}

#[tokio::test]
async fn discard_clears_pending_recovery_without_launching_an_agent() {
    run_isolated_test("discard_clears_pending_recovery_without_launching_an_agent").await;
}

async fn discard_clears_pending_recovery_without_launching_an_agent_body() {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    assert_ne!(
        bounded(resume_isolated(&manager, &workspace, true))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    bounded(manager.stop_workspace(&SpaceSelector::id(&workspace.id), StopRequest::manual()))
        .await
        .unwrap();
    bounded(launched).await.unwrap().unwrap();
    // Discard does not depend on valid current restore configuration.
    fs::write(
        workspace.path.join(".shoal.toml"),
        "[agents.fixture]\nresume_command = ['./missing', '{diff_base}']\n",
    )
    .unwrap();
    assert_eq!(
        bounded(resume_isolated(&manager, &workspace, true))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert!(!crate::execution::recovery::pending(&manager.paths, &workspace.id).unwrap());
    assert!(!workspace.path.join("restored").exists());
    assert!(workspace.path.is_dir());
    serving.abort();
}

#[tokio::test]
async fn manual_resume_consumes_the_record_on_start_even_if_other_commands_run_or_restore_fails() {
    run_isolated_test(
        "manual_resume_consumes_the_record_on_start_even_if_other_commands_run_or_restore_fails",
    )
    .await;
}

async fn manual_resume_consumes_the_record_on_start_even_if_other_commands_run_or_restore_fails_body()
 {
    let (_root, manager, workspace, serving) = fixture().await;
    let launched = launch(&manager, &workspace);
    wait_started(&manager, &workspace).await;
    assert!(
        manager
            .stop_agent_for_overload("test memory pressure")
            .await
    );
    wait_paused(&manager, &workspace).await;
    bounded(manager.stop_workspace(&SpaceSelector::id(&workspace.id), StopRequest::manual()))
        .await
        .unwrap();
    bounded(launched).await.unwrap().unwrap();
    let directory = manager.paths.workspace_state(&workspace.id);
    let record = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.to_string_lossy().ends_with(".recovery.json"))
        .unwrap();
    assert_eq!(
        fs::metadata(&record).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::write(
        workspace.path.join("resume.sh"),
        "#!/bin/sh\nprintf changed > restored\nexit 7\n",
    )
    .unwrap();
    let unrelated = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    assert_eq!(
        bounded(resume_isolated(&manager, &workspace, false))
            .await
            .unwrap()
            .unwrap(),
        7
    );
    assert_eq!(
        fs::read_to_string(workspace.path.join("restored")).unwrap(),
        "changed"
    );
    assert!(!record.exists());
    manager
        .finish_execution(unrelated.plan.id, ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    serving.abort();
}
