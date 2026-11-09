//! The restarts of one agent, a workspace's or a chat's: the one in
//! progress, which answers each request for it, and how each that ended
//! did, by the run that asked for it.
use std::collections::HashMap;

use anyhow::{Result, anyhow, ensure};
use tokio::sync::oneshot;

use super::RestartWaits;
use crate::{config::permissions::PermissionsConfig, model::PendingRestart, sessions::Origin};

/// How a restart ended: the place of the wrapper that runs the agent again,
/// when it runs in a terminal host, or why it does not run again.
pub(crate) type Outcome = Result<Option<Origin>, String>;

/// A restart in progress, which answers each request for it.
pub(crate) struct Restart {
    /// The requests still to answer; a request for an agent already
    /// restarting joins them.
    pub(super) answers: Vec<oneshot::Sender<Result<Option<Origin>>>>,
    /// The `shoal restart` run that asked for it.
    run: Option<String>,
    /// The other runs whose requests joined it, for which it restarts the
    /// agent as well.
    joined: Vec<String>,
    /// The wrapper starts the agent again itself rather than finishing for
    /// a new wrapper to start it.
    in_place: bool,
    /// The narrowing the restarted agent runs under instead of the one it
    /// recorded.
    narrowing: Option<PermissionsConfig>,
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
            narrowing: None,
            resumed: false,
            waits: None,
        }
    }

    /// This restart, with the restarted agent running under `narrowing`
    /// when given instead of the one it recorded.
    pub(crate) fn replacing(self, narrowing: Option<PermissionsConfig>) -> Self {
        Self { narrowing, ..self }
    }

    /// The narrowing the restarted agent runs under instead of the one it
    /// recorded, if any.
    pub(crate) fn narrowing(&self) -> Option<PermissionsConfig> {
        self.narrowing
    }

    /// Refuse a request for a restart under `narrowing` that this one, in
    /// progress for the agent `name`, does not replace the recorded one
    /// with; a request without one takes this restart as it is.
    pub(crate) fn ensure_replaces(
        &self,
        name: &str,
        narrowing: Option<PermissionsConfig>,
    ) -> Result<()> {
        ensure!(
            narrowing.is_none() || narrowing == self.narrowing,
            "{name} restarts already under another narrowing; restart it again once it runs"
        );
        Ok(())
    }

    /// Answer one more request, of `run`: at once while the restart waits
    /// for the agent's processes, otherwise once it ends.
    pub(crate) fn answer(
        &mut self,
        run: Option<String>,
    ) -> oneshot::Receiver<Result<Option<Origin>>> {
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
    ended: HashMap<Option<String>, Outcome>,
}

impl Restarts {
    /// The restart in progress, as `shoal restart` reads it.
    pub(crate) fn pending(&self) -> Option<PendingRestart> {
        self.current.as_ref().map(Restart::pending)
    }

    /// Where a restart of `run` started the agent again, if it did already.
    pub(crate) fn restarted_for(&self, run: &Option<String>) -> Option<Option<Origin>> {
        run.as_ref()?;
        self.ended.get(run)?.clone().ok()
    }

    /// Answer a request of `run` that joins the restart `of_run` asked for,
    /// or, once that ended, how it did, which also counts for `run`. A
    /// restart whose run a daemon before this one forgot matches whichever
    /// is in progress; `name` names the agent.
    pub(crate) fn join(
        &mut self,
        name: &str,
        of_run: &Option<String>,
        run: Option<String>,
    ) -> Result<oneshot::Receiver<Result<Option<Origin>>>, Outcome> {
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

    /// End the restart in progress with `outcome` if `ends` says it ended,
    /// answering each request for it.
    pub(crate) fn finish_if(&mut self, ends: impl FnOnce(&Restart) -> bool, outcome: &Outcome) {
        let Some(restart) = self.current.take_if(|restart| ends(restart)) else {
            return;
        };
        self.end(&restart, outcome);
        for done in restart.answers {
            // The request may have gone with its client.
            let _ = done.send(outcome.clone().map_err(|reason| anyhow!(reason)));
        }
    }

    /// Record `outcome` for every run `restart` restarted the agent for.
    fn end(&mut self, restart: &Restart, outcome: &Outcome) {
        for run in restart.runs() {
            self.ended.insert(run, outcome.clone());
        }
    }

    /// How the restart of `of_run` that ended did, which also counts for
    /// `run`, joining it late.
    fn joined_late(&mut self, name: &str, of_run: &Option<String>, run: Option<String>) -> Outcome {
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
    pub(crate) fn restarted_by(&self) -> Vec<String> {
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
pub(crate) async fn restarted(
    answer: oneshot::Receiver<Result<Option<Origin>>>,
) -> Result<Option<Origin>> {
    answer
        .await
        .unwrap_or_else(|_| Err(anyhow!("the agent did not start again")))
}
