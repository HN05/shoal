//! Whether a terminal shows the agent a wrapper runs, which the wrapper
//! reports to the daemon while the agent runs.
use std::{io::IsTerminal, time::Duration};

use crate::{
    cli::terminal::Attachment, config::Effective, model::ExecutionPlan, paths::Paths,
    protocol::AgentMethod, sessions::Origin,
};

/// How often the wrapper reads whether a terminal shows the agent and
/// reports it. Unchanged readings are reported too, so a restarted daemon
/// learns them again.
const INTERVAL: Duration = Duration::from_secs(10);

/// Reports until dropped, when the agent's process ends.
pub(super) struct Monitor(tokio::task::JoinHandle<()>);

impl Drop for Monitor {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What shows the agent to the user.
#[derive(Clone)]
pub(super) enum Shown {
    /// The place a terminal host runs it in, which the host reads.
    Place(Attachment),
    /// The terminal the wrapper runs in, which no host serves: the user's
    /// own.
    Terminal,
}

impl Shown {
    /// What shows an agent running at `place`, or in this process's
    /// terminal; `None` without either, as for a detached launch, or when
    /// `settings` disable the place's host.
    pub(super) fn of(settings: &Effective, place: Option<&Origin>) -> Option<Self> {
        match place {
            Some(place) => Attachment::of(settings, place.clone()).map(Self::Place),
            None => std::io::stdin().is_terminal().then_some(Self::Terminal),
        }
    }

    /// Whether a terminal shows the agent now; `None` when its host cannot
    /// tell.
    async fn attached(&self) -> Option<bool> {
        match self {
            Self::Place(place) => place.attached().await.ok(),
            Self::Terminal => Some(true),
        }
    }
}

pub(super) fn start(paths: &Paths, plan: &ExecutionPlan, shown: Shown) -> Monitor {
    let paths = paths.clone();
    let (workspace, execution) = (plan.workspace.id.clone(), plan.id.clone());
    Monitor(tokio::spawn(async move {
        loop {
            if let Some(attached) = shown.attached().await {
                let report = AgentMethod::ReportAttached {
                    workspace: workspace.clone(),
                    execution: execution.clone(),
                    attached,
                };
                // The agent owns the terminal, so a failed report stays
                // quiet; the next one repeats it. After an upgrade, the
                // installed release relays it.
                crate::proxy::send_report(&paths, report).await;
            }
            tokio::time::sleep(INTERVAL).await;
        }
    }))
}
