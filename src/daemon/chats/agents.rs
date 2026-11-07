//! What the daemon keeps of chats' agents while it runs: their restarts,
//! which outlive the requests that asked for them, as a workspace agent's do.
use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard, PoisonError},
};

use anyhow::{Context as _, Result, ensure};
use tokio::sync::oneshot;

use crate::{
    daemon::workspace::{Outcome, Restart, Restarts},
    model::{Chat, RunningAgent},
    sessions::Origin,
};

/// The agents of chats, by chat ID.
#[derive(Default)]
pub(crate) struct ChatAgents(Mutex<HashMap<String, ChatAgent>>);

#[derive(Default)]
struct ChatAgent {
    restarts: Restarts,
    /// The space whose process asked for the restart in progress, which
    /// stops the chat.
    stopped_by: Option<String>,
}

/// A request to restart a chat's agent.
pub(super) struct Requested {
    /// Answers the request once the restart ends, with the place the chat
    /// runs in again.
    pub answer: oneshot::Receiver<Result<Option<Origin>>>,
    /// The chat the request starts a restart of, which none was in progress
    /// for.
    pub starts: Option<Chat>,
}

impl ChatAgents {
    /// Ask for a restart of the chat `id`, running as `chat` unless it is
    /// stopped, for `run`: join the restart in progress, answer at once when
    /// another command of `run` restarted it already, or start one when its
    /// agent `resumes` its session.
    pub(super) fn request(
        &self,
        id: &str,
        chat: Option<&Chat>,
        resumes: bool,
        run: Option<String>,
    ) -> Result<Requested> {
        let mut agents = self.lock();
        if let Some(agent) = agents.get_mut(id) {
            if let Some(place) = agent.restarts.restarted_for(&run) {
                let (done, answer) = oneshot::channel();
                let _ = done.send(Ok(place));
                return Ok(Requested {
                    answer,
                    starts: None,
                });
            }
            if let Some(restart) = &mut agent.restarts.current {
                return Ok(Requested {
                    answer: restart.answer(run),
                    starts: None,
                });
            }
        }
        let chat = chat.with_context(|| format!("no running chat {id}"))?;
        ensure!(
            resumes,
            "the agent of chat {} cannot resume its session",
            chat.name
        );
        let agent = agents.entry(chat.id.clone()).or_default();
        let mut restart = Restart::unanswered(run, false);
        let answer = restart.answer(None);
        agent.restarts.current = Some(restart);
        Ok(Requested {
            answer,
            starts: Some(chat.clone()),
        })
    }

    /// Join the restart of the chat `id`, named `name`, that `of_run` asked
    /// for, for `run`, or answer as it ended, as a workspace agent's restart
    /// is joined.
    pub(super) fn join(
        &self,
        id: &str,
        name: &str,
        of_run: &Option<String>,
        run: Option<String>,
    ) -> Result<oneshot::Receiver<Result<Option<Origin>>>, Outcome> {
        match self.lock().get_mut(id) {
            Some(agent) => agent.restarts.join(name, of_run, run),
            None => Err(Err(format!("no restart of {name} is in progress"))),
        }
    }

    /// End the restart of the chat `id` in progress with `outcome`.
    pub(super) fn finish(&self, id: &str, outcome: &Outcome) {
        if let Some(agent) = self.lock().get_mut(id) {
            agent.restarts.finish_if(|_| true, outcome);
        }
    }

    /// The agents of `chats` that run, or that a restart stopped to start
    /// again, as `shoal restart` lists agents, each by its chat's ID and
    /// with whether it resumes its session.
    pub(super) fn running(&self, chats: Vec<(Chat, bool)>) -> Vec<RunningAgent> {
        let agents = self.lock();
        chats
            .into_iter()
            .filter_map(|(chat, resumes)| {
                let agent = agents.get(&chat.id);
                let restart = agent.and_then(|agent| agent.restarts.pending());
                if chat.restore.is_some() && restart.is_none() {
                    return None;
                }
                Some(RunningAgent {
                    agent: Some(chat.agent),
                    // A chat its restart saved stays restorable, so that a
                    // run joins that restart rather than skipping it.
                    restorable: resumes || restart.is_some(),
                    state: chat.agent_state.as_ref().map(|status| status.state),
                    interrupted: false,
                    origin: chat.origin,
                    restarted_by: agent
                        .map(|agent| agent.restarts.restarted_by())
                        .unwrap_or_default(),
                    restart,
                    stopped: None,
                    restarted_from: Vec::new(),
                    execution: chat.id.clone(),
                    space_id: chat.id,
                })
            })
            .collect()
    }

    /// Record that the process of the space `stopped_by` asked for the
    /// restart of the chat `id` that starts now.
    pub(super) fn set_stopper(&self, id: &str, stopped_by: Option<String>) {
        if let Some(agent) = self.lock().get_mut(id) {
            agent.stopped_by = stopped_by;
        }
    }

    /// The space whose process asked for the restart of the chat `id` in
    /// progress, when one did.
    pub(super) fn stopper(&self, id: &str) -> Option<String> {
        let agents = self.lock();
        let agent = agents.get(id)?;
        agent.restarts.current.as_ref()?;
        agent.stopped_by.clone()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, ChatAgent>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests;
