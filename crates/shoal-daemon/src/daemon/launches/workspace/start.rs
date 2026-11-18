//! The planned workspace a launch opens, recorded at once with its agent
//! queued there, waiting for its preparation and for what the launch names
//! or its issue's forge records as blocking it, which the preparation waits
//! for too before it creates the worktree.
use shoal_protocol::launches::{Launch, LaunchSource, Launched};
use std::sync::Arc;

use anyhow::Result;

use super::{ManagerReads, plan::PlannedAgent};
use crate::{
    daemon::{
        launches::shape::Shape,
        operations::starts::{Opening, Preparing},
        scope::{Caller, ScopeRefusal},
        workspace::{Manager, NewWorkspace},
    },
    forge::items::issue::IssueDetails,
    model::{Blocker, Repository},
    protocol::operations::LaunchRequest,
    start_plan::Creation,
};

/// A launch of a new workspace once planned: the workspace it creates and
/// the agent it starts there on its issue, held by `blockers`.
pub(super) struct Planned {
    pub repo: Repository,
    pub creation: Creation,
    pub agent: PlannedAgent,
    pub issue: Option<IssueDetails>,
    pub blockers: Vec<Blocker>,
}

impl Manager {
    /// Start the `planned` workspace of `launch`, which `source` made in
    /// `shape`, for `caller`, its agent waiting for what blocks it.
    pub(super) async fn start_planned(
        self: &Arc<Self>,
        launch: Launch,
        shape: Shape,
        mut planned: Planned,
        (source, reads): (LaunchSource, &ManagerReads<'_>),
    ) -> Result<Launched> {
        // A launch that starts now waits for nothing, what it names included.
        if launch.now {
            planned.blockers.clear();
        }
        if planned.agent.agent.is_some() && !launch.now {
            let issue = (planned.issue.as_ref()).map(super::plan::issue_url);
            if let Some(issue) = issue {
                let repository = &planned.repo.id;
                let forge = self
                    .forge_blockers(repository, &issue, reads.caller)
                    .await?;
                planned.blockers.extend(forge);
            }
        }
        (self.open_planned(launch, shape, planned, source, reads.caller))
            .await
            .map(Launched::from)
    }

    /// Open the `planned` workspace of `launch` and start preparing it,
    /// which nothing follows, with its agent queued there.
    async fn open_planned(
        self: &Arc<Self>,
        launch: Launch,
        shape: Shape,
        planned: Planned,
        source: LaunchSource,
        caller: Option<&Caller>,
    ) -> Result<crate::protocol::operations::StartedWorkspace> {
        let Planned {
            repo,
            creation,
            agent,
            issue,
            blockers,
        } = planned;
        let crate::start_plan::Opening {
            mut start,
            base,
            path,
            git_profile,
            options,
            pr,
        } = creation.opening()?;
        let mut parent = launch.parent.clone();
        if let Some(caller) = caller {
            ScopeRefusal::of(
                self.confine_workspace_start(caller, &mut parent, &mut start)
                    .await,
            )?;
        }
        let opening = Opening {
            start,
            base,
            new: NewWorkspace {
                git_profile: git_profile.as_deref(),
                path,
                options,
                ..Default::default()
            },
            parent: self.space_id(parent.as_ref()).await?,
        };
        let preparing = Preparing {
            issue: issue.as_ref().map(super::plan::issue_url),
            pr,
            agent: (agent.agent.clone())
                .map(|_| Box::new(agent_request(&launch, &shape, agent, source))),
            waiting: None,
            queue_entry: None,
            blockers,
            unfollowed: true,
            env: launch.setup_env.clone(),
        };
        self.gates.admit()?;
        self.start_workspace((&repo.id).into(), opening, preparing, caller)
            .await
    }
}

/// The agent `launch` queues in its new workspace once it is set up, which
/// `source` made.
fn agent_request(
    launch: &Launch,
    shape: &Shape,
    agent: PlannedAgent,
    source: LaunchSource,
) -> LaunchRequest {
    LaunchRequest {
        mode: Some(shape.mode.clone()),
        narrowing: launch.permissions,
        model: agent.model,
        args: launch.task.clone(),
        prompt: launch.prompt.clone(),
        fork: launch.fork.clone(),
        env: launch.env.clone(),
        now: launch.now,
        here: launch.here,
        source: Some(source),
        ..LaunchRequest::new(agent.agent)
    }
}
