//! Choosing the session an attach shows, once the agent there runs.
use anyhow::{Context as _, Result, bail, ensure};
use shoal_ids::execution_ids::ExecutionId;

use super::{Attaching, Target};
use crate::{
    client::connection::CommandPaths,
    model::{AgentResume, ResumeOutcome},
    sessions::{Origin, OriginExt as _},
    terminal::Session,
};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

/// An attach's steps for a caller that does its own way what `attaching`
/// does, calling the daemon at `paths`.
pub(super) struct Steps<'a, A> {
    pub paths: &'a CommandPaths,
    pub attaching: &'a A,
}

impl<A: Attaching> Steps<'_, A> {
    /// The session of `target` that `named` names, its only one or one
    /// picked, taken once the agent there runs. With no session named and
    /// none showing an agent, an agent that sleeps wakes first, and a
    /// stopped one when no session is left to attach to; a session where
    /// Shoal stopped the agent wakes it. For a user on another machine, a
    /// refusal names `machine`, this one.
    pub async fn awake_session(
        &self,
        target: Target,
        named: Option<String>,
        machine: Option<&str>,
    ) -> Result<(Target, Session)> {
        let sessions = target.sessions(self.paths).await?;
        let shows_agent = sessions.iter().any(|session| session.agent.is_some());
        if named.is_none() && !shows_agent && self.wake(&target, sessions.is_empty()).await? {
            return self.awaited(target, None).await;
        }
        let chosen = self.chosen(&target, named, &sessions, machine)?.clone();
        let (Some(execution), Some(protection)) = (&chosen.execution, &chosen.stopped) else {
            return Ok((target, chosen));
        };
        if !self.resume_stopped(execution).await? {
            bail!("{}", target.stopped_agent(protection));
        }
        self.awaited(target, Some(execution)).await
    }

    /// The one of `target`'s `sessions` that `named` names, else the only
    /// one [`offered`] offers, or one picked among them; for a user on
    /// another machine, a refusal names `machine`, this one.
    fn chosen<'s>(
        &self,
        target: &Target,
        named: Option<String>,
        sessions: &'s [Session],
        machine: Option<&str>,
    ) -> Result<&'s Session> {
        if let Some(name) = named {
            return sessions
                .iter()
                .find(|session| session.name == name)
                .with_context(|| format!("{} has no session {name}", target.name()));
        }
        match offered(sessions).as_slice() {
            [] => bail!("{}", target.without_sessions(machine)),
            [only] => Ok(only),
            several => self.attaching.pick_session(several),
        }
    }

    /// Wake `target`'s agent at once, whatever the agent limits and the
    /// queue's rank: start the restore that waits for it, as for an agent
    /// that sleeps, or when it has no session, which `stopped` says, wait
    /// for the agent a launch is starting there, else resume its stopped
    /// agent now. Whether it woke one.
    async fn wake(&self, target: &Target, stopped: bool) -> Result<bool> {
        if self.resume_space(target.id()).await? {
            return Ok(true);
        }
        if !stopped {
            return Ok(false);
        }
        if self.await_starting_launch(target.id()).await? {
            return Ok(true);
        }
        self.resume_now(target).await
    }

    /// Resume `target`'s agent at once, as `shoal resume --now` does, and
    /// wait for the restore or launch it starts through the queue; whether
    /// it had one to resume, refusing a resume that found only stopped
    /// commands.
    async fn resume_now(&self, target: &Target) -> Result<bool> {
        let resume = AgentResume {
            now: true,
            alone: true,
            env: crate::terminal::start_environment(),
            ..AgentResume::default()
        };
        let request = crate::client::agents::resume(SpaceSelector::id(target.id()), resume);
        let outcome = request.send(self.paths).await?;
        self.report_commands(target, &outcome);
        if !resumed(target.name(), outcome)? {
            return Ok(false);
        }
        // A restore whose agent registered already left the queue, which
        // `awaited` finds running.
        self.resume_space(target.id()).await?;
        Ok(true)
    }

    /// Tell the user of the stopped commands of `target` that its resume's
    /// `outcome` gives back, which no agent receives.
    fn report_commands(&self, target: &Target, outcome: &ResumeOutcome) {
        let (ResumeOutcome::Queued { commands, .. } | ResumeOutcome::Commands { commands }) =
            outcome
        else {
            return;
        };
        if !commands.is_empty() {
            let space = (target.number(), target.name());
            self.attaching.stopped_commands(space, commands);
        }
    }

    /// The session `target`'s agent runs in once the session this terminal
    /// showed ended, as a conversion moves the agent: where it runs again,
    /// once it does, starting the restore the conversion queued at once, as
    /// for an agent that sleeps. `None` once nothing moves it, as when it
    /// exited or a stop or a failure left it for `shoal resume`.
    pub async fn moved_session(&self, target: Target) -> Result<Option<(Target, Session)>> {
        let id = target.id().clone();
        let deadline = tokio::time::Instant::now() + super::WAIT;
        let mut told = false;
        loop {
            let target = Target::by_id(self.paths, id.clone()).await?;
            if let Some(session) = agent_session(target.sessions(self.paths).await?, None) {
                return Ok(Some((target, session)));
            }
            if !target.agent_moves(self.paths).await? {
                return Ok(None);
            }
            if self.resume_space(&id).await? {
                return self.awaited(target, None).await.map(Some);
            }
            if !std::mem::replace(&mut told, true) {
                eprintln!(
                    "shoal: {} moves to its new place; attaching there once its agent runs",
                    target.name()
                );
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "{} has no agent session after {} seconds; shoal attach {} attaches once it has one",
                target.name(),
                super::WAIT.as_secs(),
                target.number()
            );
            tokio::time::sleep(super::POLL).await;
        }
    }

    /// `target`, read again, with the session its agent, only `execution`
    /// when given, runs in once it does.
    async fn awaited(
        &self,
        target: Target,
        execution: Option<&ExecutionId>,
    ) -> Result<(Target, Session)> {
        let deadline = tokio::time::Instant::now() + super::WAIT;
        let id = target.id().clone();
        loop {
            let target = Target::by_id(self.paths, id.clone()).await?;
            if let Some(session) = agent_session(target.sessions(self.paths).await?, execution) {
                return Ok((target, session));
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "{} has no agent session after {} seconds; shoal attach {} attaches once it has one",
                target.name(),
                super::WAIT.as_secs(),
                target.number()
            );
            tokio::time::sleep(super::POLL).await;
        }
    }
}

/// Whether the resume of the agent of the space `name` started one, as its
/// `outcome` says, refusing one that found only stopped commands, which
/// an attach shows no session of.
fn resumed(name: &SpaceName, outcome: ResumeOutcome) -> Result<bool> {
    match outcome {
        ResumeOutcome::Continued { .. } | ResumeOutcome::Queued { .. } => Ok(true),
        ResumeOutcome::Commands { .. } => bail!("{name} has no stopped agent to resume"),
        ResumeOutcome::Nothing { .. } => Ok(false),
    }
}

/// `target`'s session at `place`, as an attach that lost its connection
/// finds it again, without waking or restoring its agent.
pub(super) async fn session_again(
    paths: &CommandPaths,
    target: Target,
    place: &Origin,
) -> Result<(Target, Session)> {
    let sessions = target.sessions(paths).await?;
    let found = sessions
        .into_iter()
        .find(|session| session.place.same_place(place));
    let Some(session) = found else {
        bail!("{}'s session is gone", target.name());
    };
    if let Some(protection) = &session.stopped {
        bail!("{}", target.stopped_agent(protection));
    }
    Ok((target, session))
}

/// The ones of `sessions` an attach that names none offers: those where the
/// daemon recorded an agent, when any is, rather than a place such as one an
/// earlier run of the agent left, else every one.
pub(super) fn offered(sessions: &[Session]) -> Vec<&Session> {
    let agents: Vec<_> = (sessions.iter())
        .filter(|session| session.agent.is_some())
        .collect();
    match agents.is_empty() {
        true => sessions.iter().collect(),
        false => agents,
    }
}

/// The one of `sessions` where an agent runs that Shoal has not stopped,
/// only `execution`'s when given, since a host may give the places of
/// several agents one name.
pub(super) fn agent_session(
    sessions: Vec<Session>,
    execution: Option<&ExecutionId>,
) -> Option<Session> {
    sessions.into_iter().find(|session| {
        session.agent.is_some()
            && session.stopped.is_none()
            && execution.is_none_or(|execution| session.execution.as_ref() == Some(execution))
    })
}

#[cfg(test)]
mod tests;
