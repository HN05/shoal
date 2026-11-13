use super::*;
use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::daemon::operations::fake;
use crate::daemon::test_support::{manager, repository};
use crate::model::{Repository, Workspace};
use crate::protocol::operations::{OperationKind, OperationSpec, Preparation};
use crate::protocol::{ErrorCode, Work, WorkPlace};
use crate::state::WorkspaceState;
use futures_util::FutureExt;
use std::sync::Arc;

#[tokio::test]
async fn restart_waits_for_background_work_and_executions_that_cannot_reattach() {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    assert!(quiesce(&manager).is_some());
    let operation = manager.gates.operation().await;
    assert!(quiesce(&manager).is_none());
    drop(operation);
    let quiet = quiesce(&manager).unwrap();
    assert!(manager.gates.try_operation().is_none());
    drop(quiet);
    // A restart leaves disconnected records as they are, including
    // commands from releases before reattachment.
    manager.store.run(|db| {
            db.execute_batch(
                "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/test','/test',0);
                 INSERT INTO workspaces(id,repository_id,name,path,branch,state)
                    VALUES ('workspace','repo','work','/test/work','work','ready');
                 INSERT INTO executions(id,space_id,state) VALUES ('legacy','workspace','running');
                 INSERT INTO executions(id,space_id,state) VALUES ('unknown','workspace','unknown');"
            )?;
            Ok(())
        }).await.unwrap();
    assert!(quiesce(&manager).is_some());
    // Connected setup and merges stop with the daemon.
    manager
        .executions
        .mark_unattachable(&ExecutionId::from("setup"), &SpaceId::from("workspace"));
    assert!(quiesce(&manager).is_none());
}

async fn registered(root: &std::path::Path, manager: &Manager) -> Repository {
    let path = repository(root, "repo");
    manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

async fn workspace(manager: &Manager, repository: &Repository, name: &str) -> Workspace {
    manager
        .create_workspace(&repository.id, name.into(), Some("HEAD".into()), None, None)
        .await
        .unwrap()
}

fn work_place(workspace: &Workspace) -> WorkPlace {
    WorkPlace {
        id: workspace.id.clone(),
        number: workspace.number,
        name: workspace.name.clone(),
    }
}

#[tokio::test]
async fn an_exit_names_each_workspace_it_would_cut_off_once() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let removing = workspace(&manager, &repository, "removing").await;
    let merging = workspace(&manager, &repository, "merging").await;
    let setting_up = workspace(&manager, &repository, "setting-up").await;
    let prepare = |workspace: &Workspace| OperationSpec::Prepare {
        workspace: SpaceSelector::id(&workspace.id),
        steps: Preparation::default(),
    };
    let preparing = manager.start_operation(prepare(&merging), None, None);
    let preparing = preparing.await.unwrap();
    let before_removal = manager.start_operation(prepare(&removing), None, None);
    let before_removal = before_removal.await.unwrap();
    let state = WorkspaceState::Removing;
    manager.set_state(&removing.id, state, None).await.unwrap();
    let executions = &manager.executions;
    executions.mark_unattachable(&ExecutionId::from("setup"), &setting_up.id);
    executions.mark_unattachable(&ExecutionId::from("merge"), &merging.id);

    let work = manager.work_an_exit_cuts_off().await.unwrap();
    assert_eq!(
        work,
        [
            Work::Workspace {
                workspace: work_place(&removing),
                state,
            },
            Work::Operation {
                workspace: work_place(&merging),
                operation: OperationKind::Prepare,
            },
            Work::Command {
                workspace: work_place(&setting_up),
            },
        ]
    );
    for id in [preparing, before_removal] {
        fake::take(&id).exit(0);
    }
}

#[tokio::test]
async fn an_exit_with_nothing_named_running_waits_for_requests_and_passes() {
    let (_root, manager) = manager().await;
    let work = manager.work_an_exit_cuts_off().await.unwrap();
    assert_eq!(work, [Work::Other]);
}

/// The frames the daemon sends the client of an exit request on `client`.
async fn waits(client: &mut UnixStream) -> Vec<Work> {
    match protocol::read::<Response>(client).await.unwrap().body {
        Body::ExitWaits(work) => work,
        body => panic!("expected the work the exit waits for, got {body:?}"),
    }
}

/// Wait for the exit as `when` says on a task of its own, with the client's
/// end of its connection.
fn waiting(
    manager: &Arc<Manager>,
    when: ExitWhen,
) -> (
    UnixStream,
    tokio::task::JoinHandle<(UnixStream, Option<Quiet>)>,
) {
    let (client, mut stream) = UnixStream::pair().unwrap();
    let manager = manager.clone();
    let wait = tokio::spawn(async move {
        let quiet = wait(&manager, &mut stream, 1, Exit::Restart, when);
        let quiet = quiet.await.unwrap();
        (stream, quiet)
    });
    (client, wait)
}

#[tokio::test]
async fn an_exit_that_waits_holds_back_new_work_until_the_running_work_ends() {
    let (_root, manager) = manager().await;
    let running = manager.gates.owned_operation().await;
    let (mut client, wait) = waiting(&manager, ExitWhen::Idle);
    assert_eq!(waits(&mut client).await, [Work::Other]);
    assert!(manager.gates.admit().is_err());
    assert!(manager.gates.background_pass().now_or_never().is_none());
    drop(running);
    let (_stream, quiet) = wait.await.unwrap();
    let mut quiet = quiet.expect("the daemon quiesced");
    assert!(manager.gates.try_operation().is_none());
    // A stop lets operations record what it ends, still holding back new work.
    quiet.release_operations();
    assert!(manager.gates.operation().now_or_never().is_some());
    assert!(manager.gates.background_pass().now_or_never().is_none());
    assert!(manager.gates.admit().is_err());
    drop(quiet);
    assert!(manager.gates.admit().is_ok());
}

#[tokio::test]
async fn an_exit_that_does_not_wait_refuses_named_work_and_holds_nothing_back() {
    let (root, manager) = manager().await;
    let repository = registered(root.path(), &manager).await;
    let setting_up = workspace(&manager, &repository, "setting-up").await;
    let executions = &manager.executions;
    executions.mark_unattachable(&ExecutionId::from("setup"), &setting_up.id);
    let (mut client, wait) = waiting(&manager, ExitWhen::IfIdle);
    let blocked = [Work::Command {
        workspace: work_place(&setting_up),
    }];
    assert_eq!(waits(&mut client).await, blocked);
    let (_stream, quiet) = wait.await.unwrap();
    assert!(quiet.is_none());
    assert!(manager.gates.admit().is_ok());
}

#[tokio::test]
async fn an_exit_that_does_not_wait_still_waits_for_requests_and_passes() {
    let (_root, manager) = manager().await;
    let running = manager.gates.owned_operation().await;
    let (mut client, wait) = waiting(&manager, ExitWhen::IfIdle);
    assert_eq!(waits(&mut client).await, [Work::Other]);
    drop(running);
    let (_stream, quiet) = wait.await.unwrap();
    assert!(quiet.is_some());
}

#[tokio::test]
async fn an_exit_whose_client_hangs_up_stops_holding_back_work() {
    let (_root, manager) = manager().await;
    let running = manager.gates.owned_operation().await;
    let (mut client, wait) = waiting(&manager, ExitWhen::Idle);
    assert_eq!(waits(&mut client).await, [Work::Other]);
    drop(client);
    let (_stream, quiet) = wait.await.unwrap();
    assert!(quiet.is_none());
    assert!(manager.gates.admit().is_ok());
    drop(running);
}

#[tokio::test]
async fn work_stays_held_back_until_every_exit_that_waits_lets_go() {
    let (_root, manager) = manager().await;
    let restart = manager.gates.hold_back(Exit::Restart);
    let refused = manager.gates.admit().unwrap_err();
    assert_eq!(refused.code(), ErrorCode::Exiting);
    // No daemon follows a stop, so its refusal sends nothing to one.
    let stop = manager.gates.hold_back(Exit::Stop);
    let refused = manager.gates.admit().unwrap_err();
    assert_eq!(refused.code(), ErrorCode::OperationFailed);
    drop(stop);
    assert_eq!(manager.gates.admit().unwrap_err().exit, Exit::Restart);
    assert!(manager.gates.background_pass().now_or_never().is_none());
    drop(restart);
    assert!(manager.gates.admit().is_ok());
    assert!(manager.gates.background_pass().now_or_never().is_some());
}

#[tokio::test]
async fn a_request_waits_out_an_attempt_to_quiesce_but_not_an_exit_that_waited() {
    let (_root, manager) = manager().await;
    let attempt = manager.gates.try_quiesce().unwrap();
    let waiting = tokio::spawn({
        let manager = manager.clone();
        async move { manager.gates.operation_unless_exiting().await.is_some() }
    });
    tokio::task::yield_now().await;
    drop(attempt);
    assert!(waiting.await.unwrap());

    let quiet = manager
        .gates
        .keep_for_exit(manager.gates.try_quiesce().unwrap());
    assert!(manager.gates.operation_unless_exiting().await.is_none());
    drop(quiet);
    assert!(manager.gates.operation_unless_exiting().await.is_some());
}
