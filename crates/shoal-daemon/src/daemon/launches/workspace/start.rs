//! The planned workspace a launch opens: prepared at once with its agent
//! queued once it is set up, or, while what the launch names or its issue's
//! forge records blocks it, the whole launch waiting in the agent queue.
use shoal_protocol::launches::{Launch, LaunchSource, Launched};
use shoal_protocol::space_names::SpaceSelector;
use std::{ffi::OsString, sync::Arc};

use anyhow::{Result, ensure};

use super::{ManagerReads, plan::PlannedAgent};
use crate::{
    daemon::{
        launches::shape::Shape,
        operations::starts::{Opening, Preparing},
        scope::{Caller, ScopeRefusal},
        workspace::{Manager, NewWorkspace},
    },
    forge::items::issue::IssueDetails,
    launches::workspace,
    model::{Blocker, QueueRequest, Repository},
    protocol::operations::LaunchRequest,
    start_plan::{AddPlan, Creation, ResolvedAddAgent},
    terminal::PlaceName,
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
    /// `shape`, for `caller`: at once, or once what blocks its agent is done.
    pub(super) async fn start_planned(
        self: &Arc<Self>,
        launch: Launch,
        shape: Shape,
        mut planned: Planned,
        (source, reads): (LaunchSource, &ManagerReads<'_>),
    ) -> Result<Launched> {
        ensure!(
            planned.blockers.is_empty() || planned.agent.agent.is_some(),
            "--add-blocker queues an agent; name one with --agent"
        );
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
        match planned.blockers.is_empty() {
            true => (self.open_planned(launch, shape, planned, source, reads.caller))
                .await
                .map(|started| Launched::Workspace(Box::new(started))),
            false => {
                self.queue_planned(launch, shape, planned, source, reads)
                    .await
            }
        }
    }

    /// Open the `planned` workspace of `launch` and start preparing it,
    /// queuing its agent once it is set up.
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
            ..
        } = planned;
        let crate::start_plan::Opening {
            mut start,
            base,
            path,
            git_profile,
            options,
            pr,
        } = creation.opening(None)?;
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
            env: launch.setup_env.clone(),
        };
        self.gates.admit()?;
        self.start_workspace((&repo.id).into(), opening, preparing, caller)
            .await
    }

    /// Queue the whole `planned` launch until what blocks its agent is done,
    /// when the agent queue starts its plan as it does any admitted launch.
    async fn queue_planned(
        &self,
        launch: Launch,
        shape: Shape,
        planned: Planned,
        source: LaunchSource,
        reads: &ManagerReads<'_>,
    ) -> Result<Launched> {
        let mut request = self
            .queued_plan_request(&launch, shape, planned, source, reads)
            .await?;
        if let Some(caller) = reads.caller {
            ScopeRefusal::of(self.queue_target(caller, &mut request).await.map(drop))?;
        }
        let admission = self.queue_own_launch(request, reads.caller).await?;
        let entry = (admission.entry)
            .ok_or_else(|| anyhow::anyhow!("the daemon queued the agent without an entry"))?;
        Ok(Launched::Waiting {
            entry: Box::new(entry),
            waiting: admission.waiting,
        })
    }

    /// The queue request of the `planned` launch of `launch`, whose plan the
    /// queue's start carries out as `shoal start` would.
    async fn queued_plan_request(
        &self,
        launch: &Launch,
        shape: Shape,
        planned: Planned,
        source: LaunchSource,
        reads: &ManagerReads<'_>,
    ) -> Result<QueueRequest> {
        let Planned {
            repo,
            mut creation,
            agent,
            issue,
            blockers,
        } = planned;
        // Without a name of its own, the workspace takes the issue's title.
        if let Some(issue) = &issue {
            (creation.options.name).get_or_insert_with(|| {
                shoal_protocol::space_names::SpaceName::derive(&issue.title, "workspace")
            });
        }
        let name = workspace::planned_name(&repo, &creation).await?;
        let opens = workspace::reopened_workspace(reads, &repo, &creation).await?;
        let priority = creation.options.priority;
        let agent_name = agent
            .agent
            .clone()
            .ok_or_else(|| anyhow::anyhow!("a waiting launch starts an agent"))?;
        let plan = AddPlan {
            repository: (&repo.id).into(),
            place_name: issue
                .as_ref()
                .map(|issue| PlaceName::new(None, &repo, Some((issue.number.get(), &issue.title)))),
            creation,
            issue: issue.as_ref().map(|issue| issue.url.clone()),
            agent: ResolvedAddAgent {
                agent: agent.agent,
                model: agent.model,
            },
            args: launch.task.iter().map(OsString::from).collect(),
            prompt: None,
            mode: Some(shape.mode.clone()),
            narrowing: launch.permissions,
            fork: launch.fork.clone(),
            queue_entry: None,
            chat: None,
            parent: launch.parent.clone(),
            workspace: None,
        };
        Ok(QueueRequest {
            repository: (&repo.id).into(),
            name,
            agent: agent_name,
            mode: Some(shape.mode),
            narrowing: launch.permissions,
            issue_title: issue.as_ref().map(|issue| issue.title.clone()),
            issue_labels: issue.map_or_else(Vec::new, |issue| issue.labels),
            plan: serde_json::to_string(&plan)?,
            env: (launch.env.clone())
                .unwrap_or_else(|| crate::terminal::detached_start_environment(None)),
            now: false,
            workspace: opens.map(SpaceSelector::Id),
            starts_in_caller: false,
            blockers,
            priority,
            source: Some(source),
        })
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
        fork: launch.fork.clone(),
        env: launch.env.clone(),
        now: launch.now,
        source: Some(source),
        ..LaunchRequest::new(agent.agent)
    }
}
