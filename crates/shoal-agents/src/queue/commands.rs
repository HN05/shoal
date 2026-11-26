//! The admission of `shoal run` commands: each waits in the agent queue at
//! the memory its earlier runs estimate, holds its admission while it runs,
//! reports what it holds, and once it ends reports what it used, which the
//! runs table keeps and the space's log shows.
use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, ensure};
use shoal_daemon_base::{
    events::record_command_ran,
    run_usage::{RunKey, RunUsage, expected_memory, record_run},
};
use shoal_ids::{execution_ids::ExecutionId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    events::CommandRanEvent,
    model::HeldBy,
    runs::{AdmissionFrame, AdmitAs, CommandKey, RunReport},
    space_names::SpaceSelector,
    time::UnixTime,
};
use tokio::{
    io::BufReader,
    net::{UnixStream, unix::OwnedReadHalf},
    sync::mpsc,
};

use super::{
    state::{Measured, MeasuredKind, OneShotAdmission, RunProcesses, Serving},
    store::last_seq,
};
use crate::{
    Agents,
    daemon::Daemon,
    protocol::{self, Body, ErrorCode, Response},
};

/// How often a waiting command learns what holds it.
const HELD_POLL: Duration = Duration::from_secs(2);

/// A command's request for admission.
pub struct CommandRequest {
    pub space: SpaceSelector,
    pub command: CommandKey,
    /// Admit it at once, whatever the limits.
    pub now: bool,
    pub admit_as: AdmitAs,
}

/// What a command's admission is for: the space it runs in, the repository
/// whose runs estimate it, and what they estimate.
struct Admitting {
    space: SpaceId,
    repository: RepositoryId,
    expected: u64,
}

/// Admit the command `request` names, then serve its admission on `stream`
/// until it reports its end or hangs up.
pub async fn admit_command(
    mut stream: UnixStream,
    request_id: u64,
    agents: Arc<Agents>,
    daemon: Arc<dyn Daemon>,
    request: CommandRequest,
    startup: tokio::sync::OwnedRwLockReadGuard<()>,
) -> Result<()> {
    let peer = stream.peer_cred().ok().and_then(|peer| peer.pid());
    let admitting = match agents.admitting(&*daemon, &request, peer).await {
        Ok(admitting) => admitting,
        Err(error) => {
            let body = Body::error(ErrorCode::OperationFailed, format!("{error:#}"));
            return protocol::write(&mut stream, &Response::new(request_id, body)).await;
        }
    };
    protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
    drop(startup);
    let (stops, stopped) = mpsc::unbounded_channel();
    let admission = agents.request_command(&admitting, &request, stops).await?;
    let (reader, writer) = stream.into_split();
    let mut link = Link {
        writer,
        reports: reports(reader),
        stopped,
    };
    let ran = link.serve(&agents, admission).await?;
    if let Some(ran) = ran {
        agents
            .record_command(&admitting, request.command, ran)
            .await;
    }
    Ok(())
}

/// The connection of an admitted or waiting command.
struct Link {
    writer: tokio::net::unix::OwnedWriteHalf,
    reports: mpsc::Receiver<Result<RunReport>>,
    /// Why protection stopped the command, once it did.
    stopped: mpsc::UnboundedReceiver<String>,
}

impl Link {
    /// Tell the command what holds it until it is admitted, then serve its
    /// admission, returning its run once it reports one, or none when it
    /// hung up first.
    async fn serve(
        &mut self,
        agents: &Agents,
        mut admission: OneShotAdmission<'_>,
    ) -> Result<Option<RunReport>> {
        if !self.wait(agents, &mut admission).await? {
            return Ok(None);
        }
        protocol::write(&mut self.writer, &AdmissionFrame::Admitted).await?;
        loop {
            tokio::select! {
                Some(reason) = self.stopped.recv() => {
                    protocol::write(&mut self.writer, &AdmissionFrame::Stop { reason }).await?;
                }
                report = self.reports.recv() => match report {
                    Some(Ok(RunReport::Using { memory })) => admission.using(memory),
                    Some(Ok(RunReport::Registered { execution })) => {
                        admission.runs_as(RunProcesses::Execution(execution));
                    }
                    Some(Ok(ran @ RunReport::Ran { .. })) => return Ok(Some(ran)),
                    Some(Err(_)) | None => return Ok(None),
                },
            }
        }
    }

    /// Wait until `admission` lets the command start, telling it what
    /// holds it whenever that changes; false when it hung up or was
    /// cancelled first.
    async fn wait(
        &mut self,
        agents: &Agents,
        admission: &mut OneShotAdmission<'_>,
    ) -> Result<bool> {
        let id = admission.id();
        let mut told: Option<HeldBy> = None;
        loop {
            tokio::select! {
                () = admission.admitted() => return Ok(true),
                Some(reason) = self.stopped.recv() => {
                    protocol::write(&mut self.writer, &AdmissionFrame::Stop { reason }).await?;
                    return Ok(false);
                }
                // A waiting command reports nothing; anything read is its end.
                _ = self.reports.recv() => return Ok(false),
                () = tokio::time::sleep(HELD_POLL) => {
                    let held = agents.agent_queue.held_back(&id);
                    if held.is_some() && held != told {
                        let frame = AdmissionFrame::Waiting { held: held.clone() };
                        protocol::write(&mut self.writer, &frame).await?;
                        told = held;
                    }
                }
            }
        }
    }
}

/// The reports `reader` sends, until it fails or hangs up.
fn reports(reader: OwnedReadHalf) -> mpsc::Receiver<Result<RunReport>> {
    let (sender, receiver) = mpsc::channel(4);
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        loop {
            let report = protocol::read_buffered::<RunReport>(&mut reader).await;
            let failed = report.is_err();
            if sender.send(report).await.is_err() || failed {
                break;
            }
        }
    });
    receiver
}

impl Agents {
    /// What admitting the command `request` names is for: its space, that
    /// space's repository, and the memory its earlier runs there estimate,
    /// none before it has run. A run admitted as running must come from its
    /// execution's wrapper, the `peer` process.
    async fn admitting(
        &self,
        daemon: &dyn Daemon,
        request: &CommandRequest,
        peer: Option<i32>,
    ) -> Result<Admitting> {
        let space = daemon.space(&request.space).await?;
        if let AdmitAs::Running { execution } = &request.admit_as {
            self.ensure_running_wrapper(execution, &space.id, peer)
                .await?;
        }
        let repository = (space.repository_id)
            .with_context(|| format!("{} belongs to no repository", space.name))?;
        let key = RunKey::Command(request.command.clone());
        let estimated = repository.clone();
        let expected = (self.store)
            .read(move |tx| expected_memory(tx, &estimated, &key))
            .await?;
        Ok(Admitting {
            space: space.id,
            repository,
            expected: expected.unwrap_or(0),
        })
    }

    /// Refuse a run admitted as the running `execution` in `space` unless
    /// the `peer` asking is that command's wrapper, which alone skips the
    /// wait that `now` needs the `limits` permission for.
    async fn ensure_running_wrapper(
        &self,
        execution: &ExecutionId,
        space: &SpaceId,
        peer: Option<i32>,
    ) -> Result<()> {
        let id = execution.clone();
        let record = (self.store)
            .read(move |tx| shoal_store::find_execution(tx, &id))
            .await?;
        let wrapper = record
            .filter(|record| record.space_id == *space && record.agent.is_none())
            .and_then(|record| record.wrapper);
        let peer = peer.and_then(|peer| u32::try_from(peer).ok());
        ensure!(
            wrapper.is_some_and(|wrapper| peer == Some(wrapper.pid)),
            "only the wrapper of command {execution} admits it as running"
        );
        Ok(())
    }

    /// Ask the agent queue to admit the command `request` names, for
    /// `admitting`, at once when it asks `now` or runs already; its stop
    /// sends protection's reason on `stops`.
    async fn request_command(
        &self,
        admitting: &Admitting,
        request: &CommandRequest,
        stops: mpsc::UnboundedSender<String>,
    ) -> Result<OneShotAdmission<'_>> {
        let queued_after = self.store.read(|tx| last_seq(tx)).await?;
        let serving = Serving {
            space: admitting.space.clone(),
            queued_after,
        };
        let measured = Measured {
            kind: MeasuredKind::Command(request.command.to_string()),
            expected: admitting.expected,
        };
        let after_stop = request.admit_as == AdmitAs::AfterStop;
        let admission = self.agent_queue.request_measured(
            &admitting.repository,
            Some(serving),
            measured,
            after_stop,
            move |reason: &str| drop(stops.send(reason.to_owned())),
        );
        if let AdmitAs::Running { execution } = &request.admit_as {
            admission.runs_as(RunProcesses::Execution(execution.clone()));
        }
        if request.now || matches!(request.admit_as, AdmitAs::Running { .. }) {
            admission.admit_now();
        }
        Ok(admission)
    }

    /// Keep the run `ran` of `command` for its estimate and the space's
    /// log, which a failure only logs.
    async fn record_command(&self, admitting: &Admitting, command: CommandKey, ran: RunReport) {
        let RunReport::Ran {
            peak,
            duration_ms,
            whole,
            oom_killed,
        } = ran
        else {
            return;
        };
        let run = RunUsage {
            ended_at: UnixTime::now(),
            peak_memory: peak,
            duration: Duration::from_millis(duration_ms),
            whole,
            oom_killed,
        };
        let event = CommandRanEvent {
            command: command.clone(),
            peak,
            duration_ms,
            whole,
            oom_killed,
        };
        let (space, repository) = (admitting.space.clone(), admitting.repository.clone());
        let recorded = self
            .store
            .write(move |tx| {
                record_run(tx, &repository, &RunKey::Command(command), &run)?;
                record_command_ran(tx, &space, &event)
            })
            .await;
        if let Err(error) = recorded {
            crate::log!(
                "run of a command in {} not recorded: {error:#}",
                admitting.space
            );
        }
    }
}

#[cfg(test)]
mod tests;
