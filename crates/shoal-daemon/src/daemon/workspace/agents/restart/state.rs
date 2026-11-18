//! The restart of one agent, a workspace's or a chat's, in progress, which
//! answers each request for it.
use anyhow::{Result, anyhow, ensure};
use tokio::sync::oneshot;

use super::RestartWaits;
use crate::{config::permissions::PermissionChange, sessions::Origin};

/// How a restart ended: the place of the wrapper that runs the agent again,
/// when it runs in a terminal host, or why it does not run again.
pub(crate) type Outcome = Result<Option<Origin>, String>;

/// A restart in progress, which answers each request for it.
pub(crate) struct Restart {
    /// The requests still to answer; a request for an agent already
    /// restarting joins them.
    pub(super) answers: Vec<oneshot::Sender<Result<Option<Origin>>>>,
    /// The wrapper starts the agent again itself rather than finishing for
    /// a new wrapper to start it.
    in_place: bool,
    /// What the restart changes in the permissions the agent was given.
    change: Option<PermissionChange>,
    /// The stopped agent was resumed, in its wrapper or a new one, so that
    /// wrapper's next start is the restarted agent rather than the first one.
    pub(super) resumed: bool,
    /// What keeps the stopped agent's processes from being proven ended,
    /// while the restart waits for them, which answers each request at once.
    pub(super) waits: Option<String>,
}

impl Restart {
    /// A restart `in_place` or in a new wrapper, making `change` when given
    /// to the permissions the agent was given, without requests to answer,
    /// such as one whose request ended with the daemon that took it.
    pub(crate) fn new(in_place: bool, change: Option<PermissionChange>) -> Self {
        Self {
            answers: Vec::new(),
            in_place,
            change,
            resumed: false,
            waits: None,
        }
    }

    /// What the restart changes in the permissions the agent was given, if
    /// anything.
    pub(crate) fn change(&self) -> Option<&PermissionChange> {
        self.change.as_ref()
    }

    /// Refuse a request for a restart making `change` that this one, in
    /// progress for the agent `name`, does not make; a request without one
    /// takes this restart as it is.
    pub(crate) fn ensure_changes(
        &self,
        name: &str,
        change: Option<&PermissionChange>,
    ) -> Result<()> {
        ensure!(
            change.is_none() || change == self.change.as_ref(),
            "{name} restarts already with another --permission; restart it again once it runs"
        );
        Ok(())
    }

    /// Answer one more request: at once while the restart waits for the
    /// agent's processes, otherwise once it ends.
    pub(crate) fn answer(&mut self) -> oneshot::Receiver<Result<Option<Origin>>> {
        let (done, answer) = oneshot::channel();
        match &self.waits {
            Some(waits) => {
                let _ = done.send(Err(RestartWaits(waits.clone()).into()));
            }
            None => self.answers.push(done),
        }
        answer
    }

    pub(crate) fn resume(&mut self) {
        self.resumed = true;
        self.waits = None;
    }

    /// Whether the wrapper starts the agent again itself.
    pub(super) fn in_place(&self) -> bool {
        self.in_place
    }

    /// Whether the agent was resumed.
    pub(super) fn resumed(&self) -> bool {
        self.resumed
    }

    /// Answer each request for it with `outcome`.
    fn end(self, outcome: &Outcome) {
        for done in self.answers {
            // The request may have gone with its client.
            let _ = done.send(outcome.clone().map_err(|reason| anyhow!(reason)));
        }
    }
}

/// An agent's restart in progress.
#[derive(Default)]
pub(crate) struct Restarts {
    /// Set while `shoal restart` restarts the agent.
    pub(crate) current: Option<Restart>,
}

impl Restarts {
    /// Drop the restart in progress, answering that it was cancelled.
    pub(crate) fn cancel(&mut self) {
        if let Some(restart) = self.current.take() {
            restart.end(&Err("the restart was cancelled".into()));
        }
    }

    /// End the restart in progress with `outcome` if `ends` says it ended,
    /// answering each request for it.
    pub(crate) fn finish_if(&mut self, ends: impl FnOnce(&Restart) -> bool, outcome: &Outcome) {
        if let Some(restart) = self.current.take_if(|restart| ends(restart)) {
            restart.end(outcome);
        }
    }
}

/// How the restart `answer` answers ended.
pub(crate) async fn restarted(
    answer: oneshot::Receiver<Result<Option<Origin>>>,
) -> Result<Option<Origin>> {
    answer
        .await
        .unwrap_or_else(|_| Err(anyhow!("the agent did not start again")))
}
