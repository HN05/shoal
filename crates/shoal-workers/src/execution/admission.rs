//! A `shoal run` command's admission: the command waits until the daemon
//! admits it at the memory its earlier runs estimate, telling the user what
//! it waits for, and holds its admission while it runs, reporting what it
//! holds and, as it ends, what it used. Protection's stop ends the run,
//! which runs again once it fits, up to [`STOPS`] times.
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::{
    runs::{AdmissionFrame, AdmitAs, CommandKey, RunReport},
    space_names::SpaceSelector,
    time::UnixTime,
};
use tokio::{
    sync::{oneshot, watch},
    task::JoinHandle,
};

use crate::client::{
    connection::CommandPaths, output::queue::held_by_label, queue::CommandAdmission,
};

/// How many times protection may stop a command before it gives up.
pub(super) const STOPS: u32 = 3;

/// How often the wrapper tells the daemon what its command holds.
const USING_INTERVAL: Duration = Duration::from_secs(5);

/// The first and the longest wait before the wrapper asks again for the
/// admission a daemon lost.
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_LAST: Duration = Duration::from_secs(30);

/// A `shoal run` command the daemon admits by its memory.
#[derive(Debug, Clone)]
pub struct CommandRun {
    pub key: CommandKey,
    /// Run it at once, whatever the limits.
    pub now: bool,
}

/// An admitted run, which the wrapper's supervision of it shares.
#[derive(Clone)]
pub(super) struct Admission(Arc<Shared>);

struct Shared {
    /// What the command holds now, in bytes.
    using: watch::Sender<u64>,
    /// Why protection stopped the command, once it did.
    stopped: watch::Sender<Option<String>>,
    /// Takes the run's end to the daemon.
    ended: Mutex<Option<oneshot::Sender<RunReport>>>,
    /// Holds the admission until the run's end reached the daemon.
    holder: Mutex<Option<JoinHandle<()>>>,
    /// The execution the run registered, which asks for the admission
    /// again as the running command's.
    execution: Mutex<Option<ExecutionId>>,
}

impl Admission {
    /// Wait until the daemon admits `run` in `space`, asked for `admit_as`,
    /// telling the user what holds it back meanwhile.
    pub(super) async fn wait(
        paths: &CommandPaths,
        space: &SpaceSelector,
        run: &CommandRun,
        admit_as: AdmitAs,
    ) -> Result<Self> {
        let mut link =
            CommandAdmission::request(paths, space.clone(), run.key.clone(), run.now, admit_as)
                .await?;
        tokio::select! {
            admitted = admitted(&mut link, Some(&run.key)) => admitted?,
            signal = interrupted() => bail!("{signal} while {} waited for memory", run.key),
        }
        let (using, using_now) = watch::channel(0);
        let (stopped, _) = watch::channel(None);
        let (ended, end) = oneshot::channel();
        let shared = Arc::new(Shared {
            using,
            stopped,
            ended: Mutex::new(Some(ended)),
            holder: Mutex::new(None),
            execution: Mutex::new(None),
        });
        let holding = Holding {
            paths: paths.clone(),
            space: space.clone(),
            key: run.key.clone(),
            shared: Arc::downgrade(&shared),
        };
        let holder = tokio::spawn(holding.hold(link, using_now, end));
        *shared.holder.lock().unwrap() = Some(holder);
        Ok(Self(shared))
    }

    /// The run registered as `execution`.
    pub(super) fn runs_as(&self, execution: &ExecutionId) {
        *self.0.execution.lock().unwrap() = Some(execution.clone());
    }

    /// Where the run's memory reads report what it holds now.
    pub(super) fn usage(&self) -> watch::Sender<u64> {
        self.0.using.clone()
    }

    /// What tells the run's supervision that protection stopped it.
    pub(super) fn stops(&self) -> watch::Receiver<Option<String>> {
        self.0.stopped.subscribe()
    }

    /// Why protection stopped the command, if it did.
    pub(super) fn stopped(&self) -> Option<String> {
        self.0.stopped.borrow().clone()
    }

    /// Report the run's end, `ran`, and give up its admission once the
    /// daemon has it.
    pub(super) async fn end(&self, ran: RunReport) {
        let ended = self.0.ended.lock().unwrap().take();
        if let Some(ended) = ended {
            let _ = ended.send(ran);
        }
        let holder = self.0.holder.lock().unwrap().take();
        if let Some(holder) = holder {
            let _ = holder.await;
        }
    }
}

/// Resolves with the signal's name once the user interrupts the wrapper or
/// its session ends. Once a run of the command has listened for these, they
/// no longer end the wrapper on their own.
async fn interrupted() -> &'static str {
    use tokio::signal::unix::SignalKind;
    tokio::select! {
        () = received(SignalKind::interrupt()) => "interrupted",
        () = received(SignalKind::terminate()) => "terminated",
        () = received(SignalKind::hangup()) => "hung up",
    }
}

/// Resolves once the wrapper receives a signal of `kind`, never where it
/// cannot listen for one.
async fn received(kind: tokio::signal::unix::SignalKind) {
    match tokio::signal::unix::signal(kind) {
        Ok(mut signal) => drop(signal.recv().await),
        Err(_) => std::future::pending().await,
    }
}

/// Wait for the daemon to admit the command on `link`, telling the user
/// what holds `key` back meanwhile, or quietly without a key.
async fn admitted(link: &mut CommandAdmission, key: Option<&CommandKey>) -> Result<()> {
    loop {
        match link.frame().await? {
            AdmissionFrame::Admitted => return Ok(()),
            AdmissionFrame::Waiting { held: Some(held) } => {
                if let Some(key) = key {
                    eprintln!("shoal: {key} waits for {}", held_by_label(&held));
                }
            }
            AdmissionFrame::Waiting { held: None } => {}
            AdmissionFrame::Stop { reason } => bail!("{reason}"),
        }
    }
}

/// What holds a run's admission: its daemon, space and command, and the
/// run's shared state while the run lasts.
struct Holding {
    paths: CommandPaths,
    space: SpaceSelector,
    key: CommandKey,
    shared: std::sync::Weak<Shared>,
}

impl Holding {
    /// Hold the run's admission on `link`: relay protection's stop, report
    /// what the run holds, from `using`, while it changes, and its end once
    /// `end` gives it. A link the daemon dropped, as a restart does, is
    /// asked for again as the running command's.
    async fn hold(
        self,
        mut link: CommandAdmission,
        mut using: watch::Receiver<u64>,
        mut end: oneshot::Receiver<RunReport>,
    ) {
        let mut tick = tokio::time::interval(USING_INTERVAL);
        loop {
            tokio::select! {
                ran = &mut end => {
                    if let Ok(ran) = ran {
                        let _ = link.report(ran).await;
                    }
                    return;
                }
                frame = link.frame() => match frame {
                    Ok(AdmissionFrame::Stop { reason }) => self.stop(reason),
                    Ok(_) => {}
                    Err(_) => match self.again(&mut end).await {
                        Some(again) => link = again,
                        None => return,
                    },
                },
                _ = tick.tick() => {
                    if using.has_changed().unwrap_or(false) {
                        let memory = *using.borrow_and_update();
                        let _ = link.report(RunReport::Using { memory }).await;
                    }
                }
            }
        }
    }

    /// Tell the run that protection stopped it for `reason`.
    fn stop(&self, reason: String) {
        if let Some(shared) = self.shared.upgrade() {
            shared.stopped.send_replace(Some(reason));
        }
    }

    /// The admission of the running command again, from a daemon that lost
    /// it, as its execution's once it registered one. Each failure, such as
    /// a daemon still restarting or a scope not yet reattached, is tried
    /// again after a longer wait; none once the run ends, which `end` says.
    async fn again(&self, end: &mut oneshot::Receiver<RunReport>) -> Option<CommandAdmission> {
        let mut wait = RETRY_FIRST;
        loop {
            let attempt = async {
                let mut link = self.request_again().await?;
                admitted(&mut link, None).await?;
                anyhow::Ok(link)
            };
            tokio::select! {
                _ = &mut *end => return None,
                again = attempt => if let Ok(link) = again {
                    return Some(link);
                },
            }
            tokio::select! {
                _ = &mut *end => return None,
                () = tokio::time::sleep(wait) => {}
            }
            wait = (wait * 2).min(RETRY_LAST);
        }
    }

    /// Ask for the running command's admission again, as its execution's
    /// once it registered one.
    async fn request_again(&self) -> Result<CommandAdmission> {
        let shared = self.shared.upgrade().context("the run ended")?;
        let execution = shared.execution.lock().unwrap().clone();
        let admit_as = execution.map_or(AdmitAs::New, |execution| AdmitAs::Running { execution });
        let request = CommandAdmission::request(
            &self.paths,
            self.space.clone(),
            self.key.clone(),
            false,
            admit_as,
        );
        request.await
    }
}

/// What the wrapper says as protection's stop of `key`, for `reason`,
/// ends its run, which runs again once it fits unless this was the last
/// of [`STOPS`].
pub(super) fn stopped_message(key: &CommandKey, reason: &str, stops: u32) -> String {
    let at = UnixTime::now().local_minutes();
    match stops < STOPS {
        true => format!("shoal: {key} stopped for {reason} at {at}; runs again once it fits"),
        false => {
            format!("shoal: {key} stopped for {reason} at {at}; giving up after {STOPS} stops")
        }
    }
}
