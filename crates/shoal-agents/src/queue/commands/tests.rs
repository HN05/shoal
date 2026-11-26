use std::sync::Arc;

use shoal_daemon_base::run_usage::{RunKey, expected_memory};
use shoal_protocol::{
    model::Workspace,
    protocol::{Body, Response},
    runs::{AdmissionFrame, AdmitAs, CommandKey, RunReport},
    space_names::SpaceSelector,
};
use tokio::{io::BufReader, net::UnixStream, task::JoinHandle};

use super::{CommandRequest, admit_command};
use crate::{daemon::Daemon, protocol, stand_in::Fixture, state::QueueKind};

const GIB: u64 = 1 << 30;

/// The wrapper's side of a command's admission, with the daemon's.
struct Wrapper {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    served: JoinHandle<anyhow::Result<()>>,
}

impl Wrapper {
    async fn frame(&mut self) -> AdmissionFrame {
        protocol::read_buffered(&mut self.reader).await.unwrap()
    }

    /// Report `ran` and hang up, once the daemon kept it.
    async fn end(mut self, ran: RunReport) {
        protocol::write(&mut self.writer, &ran).await.unwrap();
        drop(self.writer);
        self.served.await.unwrap().unwrap();
    }
}

fn fixture() -> impl Future<Output = Fixture> {
    Fixture::with_config("[machine.overload.disk]\nenabled = false")
}

/// Ask for admission of `shoal run test` in `workspace`, `now` or as the
/// queue admits it.
async fn admit(fixture: &Fixture, workspace: &Workspace, now: bool) -> Wrapper {
    let (response, wrapper) = request(fixture, workspace, now, AdmitAs::New).await;
    assert!(matches!(response.body, Body::Ok), "{:?}", response.body);
    wrapper
}

/// Ask for admission of `shoal run test` in `workspace` as `admit_as` says,
/// with the daemon's answer.
async fn request(
    fixture: &Fixture,
    workspace: &Workspace,
    now: bool,
    admit_as: AdmitAs,
) -> (Response, Wrapper) {
    let (client, server) = UnixStream::pair().unwrap();
    let request = CommandRequest {
        space: SpaceSelector::id(&workspace.id),
        command: CommandKey::Named("test".into()),
        now,
        admit_as,
    };
    let startup = Arc::new(tokio::sync::RwLock::new(())).read_owned().await;
    let daemon: Arc<dyn Daemon> = fixture.daemon.clone();
    let served = tokio::spawn(admit_command(
        server,
        1,
        fixture.agents.clone(),
        daemon,
        request,
        startup,
    ));
    let (reader, writer) = client.into_split();
    let mut reader = BufReader::new(reader);
    let response: Response = protocol::read_buffered(&mut reader).await.unwrap();
    let wrapper = Wrapper {
        reader,
        writer,
        served,
    };
    (response, wrapper)
}

fn whole_run(peak: u64) -> RunReport {
    RunReport::Ran {
        peak,
        duration_ms: 60_000,
        whole: true,
        oom_killed: false,
    }
}

#[tokio::test]
async fn a_command_runs_once_admitted_and_its_run_estimates_the_next() {
    let fixture = fixture().await;
    let workspace = fixture.workspace("api").await;
    let mut wrapper = admit(&fixture, &workspace, false).await;
    while fixture.agents.agent_queue.waiting_oneshots().is_empty() {
        tokio::task::yield_now().await;
    }
    (fixture.agents.dispatch_queue(&*fixture.daemon))
        .await
        .unwrap();
    assert_eq!(wrapper.frame().await, AdmissionFrame::Admitted);
    wrapper.end(whole_run(3 * GIB)).await;
    let key = RunKey::Command(CommandKey::Named("test".into()));
    let repository = workspace.repository().clone();
    let expected = (fixture.agents.store)
        .read(move |tx| expected_memory(tx, &repository, &key))
        .await
        .unwrap();
    assert_eq!(expected, Some(3 * GIB));
}

#[tokio::test]
async fn protection_stops_an_admitted_command_through_its_link() {
    let fixture = fixture().await;
    let workspace = fixture.workspace("api").await;
    let mut wrapper = admit(&fixture, &workspace, true).await;
    assert_eq!(wrapper.frame().await, AdmissionFrame::Admitted);
    let running = fixture.agents.agent_queue.running_measured();
    let [command] = running.as_slice() else {
        panic!("one command runs");
    };
    assert_eq!(command.space.as_ref(), Some(&workspace.id));
    let queue = &fixture.agents.agent_queue;
    assert!(queue.stop_measured(&command.id, "memory pressure"));
    assert!(!queue.stop_measured(&command.id, "memory pressure"));
    assert_eq!(
        wrapper.frame().await,
        AdmissionFrame::Stop {
            reason: "memory pressure".into()
        }
    );
}

#[tokio::test]
async fn a_waiting_command_is_listed_and_cancelled_in_the_queue() {
    let fixture = fixture().await;
    let workspace = fixture.workspace("api").await;
    let mut wrapper = admit(&fixture, &workspace, false).await;
    let daemon = &*fixture.daemon;
    let entries = loop {
        let entries = fixture.agents.queue_entries(daemon).await.unwrap();
        if !entries.is_empty() {
            break entries;
        }
        tokio::task::yield_now().await;
    };
    let [entry] = entries.as_slice() else {
        panic!("one entry waits");
    };
    assert_eq!(entry.kind, QueueKind::Command);
    assert_eq!(entry.command.as_deref(), Some("test"));
    assert_eq!(entry.space_id.as_ref(), Some(&workspace.id));
    let removed = fixture.agents.queue_remove(daemon, entry.id.as_str(), None);
    removed.await.unwrap();
    assert!(matches!(wrapper.frame().await, AdmissionFrame::Stop { .. }));
}

#[tokio::test]
async fn a_stop_of_its_space_cancels_a_waiting_command() {
    let fixture = fixture().await;
    let (api, web) = (
        fixture.workspace("api").await,
        fixture.workspace("web").await,
    );
    let mut stopped = admit(&fixture, &api, false).await;
    let _waiting = admit(&fixture, &web, false).await;
    let queue = fixture.agents.agent_queue();
    while queue.waiting_oneshots().len() < 2 {
        tokio::task::yield_now().await;
    }
    assert_eq!(queue.cancel_waiting_in(&api.id, "its space was stopped"), 1);
    assert!(matches!(stopped.frame().await, AdmissionFrame::Stop { .. }));
}

/// Record the running command `id` in `workspace`, whose wrapper is the
/// process `pid`.
async fn command_execution(fixture: &Fixture, workspace: &Workspace, id: &str, pid: u32) {
    let wrapper = serde_json::json!({"pid": pid, "birth": "b"}).to_string();
    let (id, space) = (id.to_owned(), workspace.id.clone());
    (fixture.agents.store)
        .write(move |tx| {
            tx.execute(
                "INSERT INTO executions(id,space_id,state,wrapper) VALUES (?1,?2,'running',?3)",
                rusqlite::params![id, space, wrapper],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn only_its_own_wrapper_admits_a_command_as_running() {
    let fixture = fixture().await;
    let workspace = fixture.workspace("api").await;
    command_execution(&fixture, &workspace, "own", std::process::id()).await;
    command_execution(&fixture, &workspace, "other", 1).await;
    let running = |execution: &str| AdmitAs::Running {
        execution: execution.into(),
    };
    let (refused, _) = request(&fixture, &workspace, false, running("other")).await;
    assert!(
        matches!(refused.body, Body::Error { .. }),
        "{:?}",
        refused.body
    );
    let (response, mut wrapper) = request(&fixture, &workspace, false, running("own")).await;
    assert!(matches!(response.body, Body::Ok), "{:?}", response.body);
    assert_eq!(wrapper.frame().await, AdmissionFrame::Admitted);
}

#[tokio::test]
async fn a_command_s_registered_execution_takes_its_place_in_the_oom_scores() {
    let fixture = fixture().await;
    let workspace = fixture.workspace("api").await;
    let mut wrapper = admit(&fixture, &workspace, true).await;
    assert_eq!(wrapper.frame().await, AdmissionFrame::Admitted);
    let registered = RunReport::Registered {
        execution: "run".into(),
    };
    protocol::write(&mut wrapper.writer, &registered)
        .await
        .unwrap();
    let queue = &fixture.agents.agent_queue;
    let id = queue.running_measured()[0].id.clone();
    let execution = crate::queue::state::RunProcesses::Execution("run".into());
    while queue.measured_processes(&id) != Some(execution.clone()) {
        tokio::task::yield_now().await;
    }
}
