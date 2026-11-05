//! The restarts of one agent: the one in progress, which answers each
//! request for it, and how each that ended did, by the run that asked for it.
use std::collections::HashMap;

use anyhow::{Result, anyhow};
use tokio::sync::oneshot;

use super::RestartWaits;
use crate::model::PendingRestart;

/// A restart in progress, which answers each request for it.
pub(crate) struct Restart {
    /// The requests still to answer; a request for an agent already
    /// restarting joins them.
    pub(super) answers: Vec<oneshot::Sender<Result<()>>>,
    /// The `shoal restart` run that asked for it.
    run: Option<String>,
    /// The other runs whose requests joined it, for which it restarts the
    /// agent as well.
    joined: Vec<String>,
    /// The wrapper starts the agent again itself rather than finishing for
    /// a new wrapper to start it.
    in_place: bool,
    /// The stopped agent was resumed, in its wrapper or a new one, so that
    /// wrapper's next start is the restarted agent rather than the first one.
    pub(super) resumed: bool,
    /// Why the restart waits for the stopped agent's processes, which
    /// answers each request at once.
    pub(super) waits: Option<String>,
}

impl Restart {
    /// A restart for `run`, `in_place` or in a new wrapper, without requests
    /// to answer, such as one whose request ended with the daemon that took
    /// it.
    pub(crate) fn unanswered(run: Option<String>, in_place: bool) -> Self {
        Self {
            answers: Vec::new(),
            run,
            joined: Vec::new(),
            in_place,
            resumed: false,
            waits: None,
        }
    }

    /// Answer one more request, of `run`: at once while the restart waits
    /// for the agent's processes, otherwise once it ends.
    pub(super) fn answer(&mut self, run: Option<String>) -> oneshot::Receiver<Result<()>> {
        if let Some(run) = run
            && self.run.as_ref() != Some(&run)
            && !self.joined.contains(&run)
        {
            self.joined.push(run);
        }
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

    /// Whether a new wrapper starts the stopped agent again.
    pub(super) fn in_new_wrapper(&self) -> bool {
        self.resumed && !self.in_place
    }

    fn pending(&self) -> PendingRestart {
        PendingRestart {
            run: self.run.clone(),
            joined: self.joined.clone(),
        }
    }

    /// Every run it restarts the agent for.
    fn runs(&self) -> impl Iterator<Item = Option<String>> + '_ {
        std::iter::once(self.run.clone()).chain(self.joined.iter().cloned().map(Some))
    }
}

/// An agent's restart in progress and those that ended.
#[derive(Default)]
pub(crate) struct Restarts {
    /// Set while `shoal restart` restarts the agent.
    pub(crate) current: Option<Restart>,
    /// How each of its restarts that ended did, by the run that asked for
    /// it, which answers a request that joins it late.
    ended: HashMap<Option<String>, Result<(), String>>,
}

impl Restarts {
    /// The restart in progress, as `shoal restart` reads it.
    pub(super) fn pending(&self) -> Option<PendingRestart> {
        self.current.as_ref().map(Restart::pending)
    }

    /// Whether a restart of `run` started the agent again already.
    pub(super) fn restarted_for(&self, run: &Option<String>) -> bool {
        run.is_some() && self.ended.get(run) == Some(&Ok(()))
    }

    /// Answer a request of `run` that joins the restart `of_run` asked for,
    /// or, once that ended, how it did, which also counts for `run`. A
    /// restart whose run a daemon before this one forgot matches whichever
    /// is in progress; `name` names the agent.
    pub(super) fn join(
        &mut self,
        name: &str,
        of_run: &Option<String>,
        run: Option<String>,
    ) -> Result<oneshot::Receiver<Result<()>>, Result<(), String>> {
        match &mut self.current {
            Some(restart) if of_run.is_none() || &restart.run == of_run => Ok(restart.answer(run)),
            _ => Err(self.joined_late(name, of_run, run)),
        }
    }

    /// Drop the restart in progress, recording that it was cancelled.
    pub(crate) fn cancel(&mut self) {
        if let Some(restart) = self.current.take() {
            self.end(&restart, &Err("the restart was cancelled".into()));
        }
    }

    /// Record `outcome` for every run `restart` restarted the agent for.
    pub(super) fn end(&mut self, restart: &Restart, outcome: &Result<(), String>) {
        for run in restart.runs() {
            self.ended.insert(run, outcome.clone());
        }
    }

    /// How the restart of `of_run` that ended did, which also counts for
    /// `run`, joining it late.
    fn joined_late(
        &mut self,
        name: &str,
        of_run: &Option<String>,
        run: Option<String>,
    ) -> Result<(), String> {
        let outcome = self
            .ended
            .get(of_run)
            .cloned()
            .unwrap_or_else(|| Err(format!("no restart of {name} is in progress")));
        if run.is_some() {
            self.ended.insert(run, outcome.clone());
        }
        outcome
    }

    /// The runs a restart started the agent again for.
    pub(super) fn restarted_by(&self) -> Vec<String> {
        let mut runs: Vec<String> = self
            .ended
            .iter()
            .filter(|(_, outcome)| outcome.is_ok())
            .filter_map(|(run, _)| run.clone())
            .collect();
        runs.sort();
        runs
    }
}

/// How the restart `answer` answers ended.
pub(super) async fn restarted(answer: oneshot::Receiver<Result<()>>) -> Result<()> {
    answer
        .await
        .unwrap_or_else(|_| Err(anyhow!("the agent did not start again")))
}
