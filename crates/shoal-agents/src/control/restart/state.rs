//! The restart of one agent, a workspace's or a chat's, in progress, which
//! answers each request for it.
use anyhow::{Result, anyhow};
use tokio::sync::oneshot;

use super::RestartWaits;
use crate::config::permissions::PermissionChange;
use shoal_core::sessions::Origin;

/// How a restart ended: the place of the wrapper that runs the agent again,
/// when it runs in a terminal host, or why it does not run again.
pub type Outcome = Result<Option<Origin>, String>;

/// A restart in progress, which answers each request for it.
pub struct Restart {
    /// The requests still to answer; a request for an agent already
    /// restarting joins them.
    pub answers: Vec<oneshot::Sender<Result<Option<Origin>>>>,
    /// The wrapper starts the agent again itself rather than finishing for
    /// a new wrapper to start it.
    in_place: bool,
    /// What the restart changes in the permissions the agent was given.
    change: Option<PermissionChange>,
    /// The wrapper took how the restart starts the agent again and with
    /// what permissions, which a newer request no longer changes.
    settled: bool,
    /// The stopped agent was resumed, in its wrapper or a new one, so that
    /// wrapper's next start is the restarted agent rather than the first one.
    pub resumed: bool,
    /// What keeps the stopped agent's processes from being proven ended,
    /// while the restart waits for them, which answers each request at once.
    pub waits: Option<String>,
}

impl Restart {
    /// A restart `in_place` or in a new wrapper, making `change` when given
    /// to the permissions the agent was given, without requests to answer,
    /// such as one whose request ended with the daemon that took it.
    pub fn new(in_place: bool, change: Option<PermissionChange>) -> Self {
        Self {
            answers: Vec::new(),
            in_place,
            change,
            settled: false,
            resumed: false,
            waits: None,
        }
    }

    /// What the restart changes in the permissions the agent was given, if
    /// anything.
    pub fn change(&self) -> Option<&PermissionChange> {
        self.change.as_ref()
    }

    /// Take a newer request for a restart `in_place` or not, making
    /// `change`, which wins over this one's on a key both set, unless the
    /// wrapper settled this one already; whether it did.
    pub fn merge(&mut self, in_place: bool, change: Option<PermissionChange>) -> bool {
        if self.settled {
            return false;
        }
        self.in_place = in_place;
        self.change = PermissionChange::then(self.change.take(), change);
        true
    }

    /// Settle how the restart starts the agent again and with what
    /// permissions, returning whether in its wrapper.
    pub fn settle(&mut self) -> bool {
        self.settled = true;
        self.in_place
    }

    /// Answer one more request: at once while the restart waits for the
    /// agent's processes, otherwise once it ends.
    pub fn answer(&mut self) -> oneshot::Receiver<Result<Option<Origin>>> {
        let (done, answer) = oneshot::channel();
        match &self.waits {
            Some(waits) => {
                let _ = done.send(Err(RestartWaits(waits.clone()).into()));
            }
            None => self.answers.push(done),
        }
        answer
    }

    pub fn resume(&mut self) {
        self.settled = true;
        self.resumed = true;
        self.waits = None;
    }

    /// Whether the agent was resumed.
    pub fn resumed(&self) -> bool {
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
pub struct Restarts {
    /// Set while `shoal restart` restarts the agent.
    pub current: Option<Restart>,
}

impl Restarts {
    /// Drop the restart in progress, answering that it was cancelled.
    pub fn cancel(&mut self) {
        if let Some(restart) = self.current.take() {
            restart.end(&Err("the restart was cancelled".into()));
        }
    }

    /// End the restart in progress with `outcome` if `ends` says it ended,
    /// answering each request for it.
    pub fn finish_if(&mut self, ends: impl FnOnce(&Restart) -> bool, outcome: &Outcome) {
        if let Some(restart) = self.current.take_if(|restart| ends(restart)) {
            restart.end(outcome);
        }
    }
}

/// How the restart `answer` answers ended.
pub async fn restarted(
    answer: oneshot::Receiver<Result<Option<Origin>>>,
) -> Result<Option<Origin>> {
    answer
        .await
        .unwrap_or_else(|_| Err(anyhow!("the agent did not start again")))
}
