//! Opening a workspace and starting its preparation in one request, as
//! `shoal start` and `shoal split` do: the checks before anything is created,
//! the workspace and its issue, then the operation that prepares it, which
//! creates a new branch's worktree first, unless it waits for the work its
//! agent waits for or for disk space, and whose failure the start's
//! coordinator or the user hears of.
mod failures;
mod waiting;

pub(crate) use waiting::{note_stopped, preparation_waits};

use crate::daemon::links::issue::{Assigner, AssignerOf};
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Context as _, Result, ensure};
use waiting::PreparationStart;

use crate::{
    agents::{check_queued_launch, resolve_launch},
    config::Effective,
    daemon::{
        links::issue::OpenIssue,
        log,
        scope::Caller,
        workspace::{CopyPlace, Manager, NewWorkspace, agent_queue::HasAgent},
    },
    forge::items::reference::ItemInput,
    model::{Blocker, OpenedWorkspace, QueueEntry, Workspace},
    protocol::{
        CopySource, CreationOptions,
        operations::{
            LaunchIssue, LaunchRequest, OperationSpec, Preparation, QueuedLaunch, StartedWorkspace,
            WorkspaceStart,
        },
    },
    removal::{InspectionPolicy, UnsavedWork},
    repository::Selector,
    state::WorkspaceState,
};
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

/// Name a new workspace without a name of its own after `issue`'s title,
/// as every start does, whoever sends it; options apply only to a new
/// workspace, so one reused keeps its name.
fn name_after_issue(options: &mut CreationOptions, issue: Option<&OpenIssue>) {
    if let Some(issue) = issue {
        (options.name).get_or_insert_with(|| SpaceName::derive(&issue.title, "workspace"));
    }
}

/// What a workspace [`Manager::start_workspace`] opens: where it starts,
/// from what base, where and with what Git profile, below which space, and
/// what it leaves out.
pub(crate) struct Opening<'a> {
    pub start: WorkspaceStart,
    pub base: Option<String>,
    pub new: NewWorkspace<'a>,
    pub parent: Option<SpaceId>,
}

/// The child a split creates: on the new branch `name` from `base`, or a
/// copy of `copy`, on `name` or a branch named after the copied one's.
pub(crate) struct Split {
    pub name: Option<String>,
    pub copy: Option<CopySource>,
    pub base: Option<String>,
    pub options: CreationOptions,
}

/// What preparing a new workspace does once it exists, in the caller's
/// environment `env`, with its agent as a request names it or, by default,
/// as the daemon resolved it.
pub(crate) struct Preparing<L = QueuedLaunch> {
    /// An issue number or URL the workspace is assigned.
    pub issue: Option<crate::forge::items::reference::ItemInput>,
    pub pr: Option<crate::forge::items::reference::ItemInput>,
    pub agent: Option<Box<L>>,
    /// The agent the caller starts itself once setup succeeds, which waits
    /// in the queue should setup fail.
    pub waiting: Option<Box<QueuedLaunch>>,
    /// The starting queue entry of the launch the start carries out, which
    /// the opened workspace takes before its preparation starts.
    pub queue_entry: Option<QueueEntryId>,
    /// What `agent` waits for besides the preparation, which waits for it
    /// too before it creates a new worktree, then creates it on the branch
    /// of the done blocker that holds the others' work.
    pub blockers: Vec<Blocker>,
    /// Whether nothing follows the preparation, which then also waits
    /// before it creates a new worktree while disk protection holds back
    /// new starts.
    pub unfollowed: bool,
    pub env: Option<BTreeMap<String, String>>,
}

impl Preparing<LaunchRequest> {
    /// This preparation with its agent resolved, and the agent the caller
    /// starts itself checked, under the `settings` they launch with, read
    /// only when the start has either.
    async fn resolved(
        self,
        settings: impl AsyncFnOnce() -> Result<Effective>,
    ) -> Result<Preparing> {
        let Self {
            issue,
            pr,
            agent,
            waiting,
            queue_entry,
            blockers,
            unfollowed,
            env,
        } = self;
        let agent = match (agent, &waiting) {
            (None, None) => None,
            (agent, waiting) => {
                let settings = settings().await?;
                if let Some(waiting) = waiting {
                    check_queued_launch(waiting, &settings)?;
                }
                let resolved = agent.map(|agent| resolve_launch(*agent, &settings));
                resolved.transpose()?.map(Box::new)
            }
        };
        Ok(Preparing {
            issue,
            pr,
            agent,
            waiting,
            queue_entry,
            blockers,
            unfollowed,
            env,
        })
    }
}

impl Preparing {
    /// Whether the preparation of `workspace` may wait before it creates
    /// the new worktree: for what its agent waits for, or, with nothing
    /// following it, for disk space.
    fn may_wait(&self, workspace: &Workspace) -> bool {
        workspace.pending_worktree && (!self.blockers.is_empty() || self.unfollowed)
    }

    /// Queue its agent on the start's `issue` unless it names a task of its
    /// own.
    fn work_on(&mut self, issue: Option<&OpenIssue>) {
        if let (Some(agent), Some(OpenIssue { url, title, labels })) = (&mut self.agent, issue) {
            agent.default_issue(LaunchIssue {
                url: url.clone(),
                title: title.clone(),
                labels: labels.clone(),
            });
        }
    }
}

impl Manager {
    /// Open the workspace `opening` names in `repository`, a new one or
    /// the one on its branch or PR head, and start preparing it.
    pub(crate) async fn start_workspace(
        self: &Arc<Self>,
        repository: Selector,
        mut opening: Opening<'_>,
        preparing: Preparing<LaunchRequest>,
        caller: Option<&Caller>,
    ) -> Result<StartedWorkspace> {
        ensure!(
            preparing.blockers.is_empty() || preparing.agent.is_some(),
            "--add-blocker queues an agent; name one with --agent"
        );
        let repo = self.repository(repository.clone()).await?;
        let start = &opening.start;
        let mut preparing = (preparing.resolved(async || {
            ensure!(
                !matches!(start, WorkspaceStart::PullHead { .. }),
                "a PR's head opens in a workspace at a commit, which holds no agent of its own"
            );
            self.start_settings(&repo.id, start).await
        }))
        .await?;
        let issue = match &preparing.issue {
            Some(issue) => Some(self.read_open_issue(&repo.path, issue).await?),
            None => None,
        };
        preparing.work_on(issue.as_ref());
        name_after_issue(&mut opening.new.options, issue.as_ref());
        if opening.new.options.name.is_none() {
            let title = self
                .opened_pull_title(&repo.path, &opening, &preparing)
                .await;
            opening.new.options.name = title.map(|title| SpaceName::derive(&title, "workspace"));
        }
        self.authorize_start_agent(caller, &preparing, &repo.id)
            .await?;
        let asked = opening.new.options.priority;
        let given = self.given_priority(caller, asked).await?;
        let opened = (self.open_for_start(repository, opening, caller)).await?;
        // A reopened workspace keeps its priority unless one was asked for.
        let priority = if opened.reused { asked } else { given };
        self.give_priority(&opened.workspace.id, priority).await?;
        let started = self.prepare_new(opened, issue, preparing, caller).await?;
        self.report_failed_preparation(&started);
        Ok(started)
    }

    /// Refuse the scoped `caller` the agent `preparing` queues in
    /// `repository`, before the start creates anything, where it gives the
    /// agent permissions the caller does not hold, or starts it regardless
    /// of the limits beyond what the caller's `limits` permission allows.
    async fn authorize_start_agent(
        &self,
        caller: Option<&Caller>,
        preparing: &Preparing,
        repository: &RepositoryId,
    ) -> Result<()> {
        let (Some(caller), Some(agent)) = (caller, &preparing.agent) else {
            return Ok(());
        };
        let (giver, given) = (Some(&caller.space_id), &agent.narrowing);
        (self.ensure_caller_holds(giver, given, Some(repository))).await?;
        if !agent.now {
            return Ok(());
        }
        let start = "start the agent of a new workspace now".to_owned();
        (self.authorize_beyond_limits(caller, start, Some(repository.clone()))).await
    }

    /// The settings the agent of a start that opens `start` in `repository`
    /// launches with: those of the workspace it names, its worktree's file
    /// included, or else the repository's.
    async fn start_settings(
        &self,
        repository: &RepositoryId,
        start: &WorkspaceStart,
    ) -> Result<Effective> {
        match start {
            WorkspaceStart::Workspace(workspace) => {
                let workspace = self.workspace(workspace).await?;
                self.workspace_settings_with_templates(&workspace).await
            }
            _ => self.repository_settings_with_templates(repository).await,
        }
    }

    /// The title of the PR a start opens at its head or links, which names
    /// a new workspace without a name or issue of its own, as every start
    /// does; a PR that cannot be read leaves it named after its branch, and
    /// linking it reports why.
    async fn opened_pull_title(
        &self,
        checkout: &std::path::Path,
        opening: &Opening<'_>,
        preparing: &Preparing,
    ) -> Option<String> {
        let input = match &opening.start {
            WorkspaceStart::PullHead { url, .. } => &url.clone().into(),
            _ => preparing.pr.as_ref()?,
        };
        match self.read_pull_title(checkout, input).await {
            Ok(title) => Some(title),
            Err(error) => {
                log!("PR {input} not read to name its workspace: {error:#}");
                None
            }
        }
    }

    /// Split child `name` from workspace `parent`, on `base` or a copy of
    /// `copy`, leaving out what `options` say, and start preparing it.
    pub(crate) async fn start_split(
        self: &Arc<Self>,
        parent: &SpaceSelector,
        mut split: Split,
        preparing: Preparing<LaunchRequest>,
        caller: Option<&Caller>,
    ) -> Result<StartedWorkspace> {
        let parent = self.workspace(parent).await?;
        let mut preparing = (preparing
            .resolved(async || self.workspace_settings_with_templates(&parent).await))
        .await?;
        let issue = match &preparing.issue {
            Some(issue) => Some(self.read_open_issue(&parent.path, issue).await?),
            None => None,
        };
        preparing.work_on(issue.as_ref());
        name_after_issue(&mut split.options, issue.as_ref());
        (self.authorize_start_agent(caller, &preparing, parent.repository())).await?;
        let priority = (self.given_priority(caller, split.options.priority)).await?;
        let child = self.create_child(&parent, split).await?;
        self.give_priority(&child.id, priority).await?;
        let opened = OpenedWorkspace {
            workspace: child,
            reused: false,
        };
        let started = self.prepare_new(opened, issue, preparing, caller).await?;
        self.report_failed_preparation(&started);
        Ok(started)
    }

    /// Create the child `split` describes below `parent`.
    async fn create_child(
        &self,
        parent: &Workspace,
        Split {
            name,
            copy,
            base,
            options,
        }: Split,
    ) -> Result<Workspace> {
        let Some(copy) = copy else {
            let name = name.context("name the child's branch")?;
            return self
                .split_workspace(&SpaceSelector::id(&parent.id), name, base, options)
                .await;
        };
        ensure!(base.is_none(), "a copy starts from its source, not a base");
        let new = NewWorkspace {
            options,
            parent: Some(parent.id.clone()),
            ..Default::default()
        };
        let repository = Selector::from(parent.repository().clone());
        (self.create_copy(repository, &copy, CopyPlace::Branch(name), new)).await
    }

    /// Record the workspace `opening` names in `repository` on a new
    /// branch, or on its existing branch, whose worktree its preparation
    /// creates, or open the one on that branch or its PR head, or the
    /// recorded one it names. For a scoped `caller`, the parent takes over
    /// no workspace outside its subtree.
    async fn open_for_start(
        &self,
        repository: Selector,
        Opening {
            start,
            base,
            new,
            parent,
        }: Opening<'_>,
        caller: Option<&Caller>,
    ) -> Result<OpenedWorkspace> {
        let placed = base.is_some() || new.path.is_some() || new.git_profile.is_some();
        // A new worktree waits for the preparation the start runs.
        let new = NewWorkspace {
            pending: !matches!(start, WorkspaceStart::Workspace(_)),
            ..new
        };
        match start {
            WorkspaceStart::Name(name) => {
                let workspace = self
                    .create_workspace_below(repository, name, base, new, parent)
                    .await?;
                Ok(OpenedWorkspace {
                    workspace,
                    reused: false,
                })
            }
            WorkspaceStart::Typed(name) => {
                let new = NewWorkspace {
                    exact_branch: true,
                    ..new
                };
                let workspace = self
                    .create_workspace_below(repository, name, base, new, parent)
                    .await?;
                Ok(OpenedWorkspace {
                    workspace,
                    reused: false,
                })
            }
            WorkspaceStart::Branch(branch) => {
                self.open_branch_below(repository, &branch, base, new, parent, caller)
                    .await
            }
            WorkspaceStart::Copy {
                source,
                branch,
                typed,
            } => {
                ensure!(base.is_none(), "a copy starts from its source, not a base");
                let new = NewWorkspace {
                    parent: parent.clone(),
                    exact_branch: typed,
                    ..new
                };
                let place = CopyPlace::Branch(branch);
                let workspace = self.create_copy(repository, &source, place, new).await?;
                Ok(OpenedWorkspace {
                    workspace: self
                        .give_to_parent(&SpaceSelector::id(&workspace.id), parent)
                        .await?,
                    reused: false,
                })
            }
            WorkspaceStart::PullHead { url, head } => {
                ensure!(
                    !placed,
                    "a workspace at a PR's head takes no base, path or Git profile"
                );
                let new = NewWorkspace { parent, ..new };
                (self.open_pull_head(repository, url, head, new, caller)).await
            }
            WorkspaceStart::Workspace(workspace) => {
                ensure!(
                    !placed && parent.is_none(),
                    "an existing workspace takes no base, path, Git profile or parent"
                );
                let repository = self.repository(repository).await?;
                let workspace = self.workspace(&workspace).await?;
                ensure!(
                    workspace.repository() == &repository.id,
                    "workspace {} is not in the requested repository",
                    workspace.name
                );
                self.reopen_workspace(&SpaceSelector::id(&workspace.id))
                    .await
            }
        }
    }

    /// Assign `issue` to the `opened` workspace, give it the start's queue
    /// entry and queue its agent, which waits for its preparation, then
    /// start preparing it: the worktree it has yet to get, its setup while
    /// it is preparing, linking the PR, then the post-setup hook of a new
    /// workspace. A preparation that creates the worktree waits first for
    /// what the agent waits for, or, with nothing following it, for disk
    /// space; a reopened workspace whose admitted launch the start carries
    /// out is refreshed first. A new workspace whose issue cannot be
    /// assigned is removed, since nothing ran in it yet.
    pub(crate) async fn prepare_new(
        self: &Arc<Self>,
        OpenedWorkspace { workspace, reused }: OpenedWorkspace,
        issue: Option<OpenIssue>,
        preparing: Preparing,
        caller: Option<&Caller>,
    ) -> Result<StartedWorkspace> {
        (self.ensure_start_agent_held(&workspace, &preparing, caller)).await?;
        let warnings = (self.assign_start_issue(&workspace, reused, issue, caller)).await?;
        // The entry is the workspace's before its setup can fail, so a
        // waiting launch frees its slot even once the caller is gone.
        if let Some(entry) = &preparing.queue_entry {
            self.give_start_entry(entry, &workspace).await?;
        }
        // The agent an admitted launch starts in an existing workspace starts
        // on what it was made from now.
        let set_up_again =
            reused && preparing.queue_entry.is_some() && self.refresh_admitted(&workspace).await;
        let may_wait = preparing.may_wait(&workspace);
        let Preparing {
            pr,
            agent,
            waiting,
            blockers,
            env,
            ..
        } = preparing;
        let opened = (&workspace, reused);
        let entry = (self.queue_start_agent(opened, agent, (pr.clone(), blockers))).await?;
        let start = PreparationStart {
            steps: Preparation {
                setup: workspace.state == WorkspaceState::Preparing || set_up_again,
                post_setup: !reused,
                pr,
                agent: None,
                waiting,
            },
            env,
            for_agent: entry.is_some(),
        };
        let (recorded, operation) =
            (self.begin_preparation(&workspace, start, may_wait, caller)).await?;
        Ok(StartedWorkspace {
            operation,
            workspace: Box::new(recorded),
            reused,
            entry,
            warnings,
        })
    }

    /// Queue `agent`, if any, in the `workspace` a start opened, `reused`
    /// or new, linking `pr` as it starts once `blockers` are done.
    async fn queue_start_agent(
        &self,
        opened: (&Workspace, bool),
        agent: Option<Box<QueuedLaunch>>,
        (pr, blockers): (Option<ItemInput>, Vec<Blocker>),
    ) -> Result<Option<Box<QueueEntry>>> {
        let Some(agent) = agent else {
            return Ok(None);
        };
        let queued = self.queue_for_preparation(opened, *agent, (pr, blockers));
        Ok(Some(Box::new(queued.await?)))
    }

    /// Start the preparation `start` of `workspace` for `caller`, unless
    /// it `may_wait` and what it waits for holds it, returning the
    /// workspace as the start left it, before its preparation changes it,
    /// and the preparation's operation once it starts.
    async fn begin_preparation(
        self: &Arc<Self>,
        workspace: &Workspace,
        start: PreparationStart,
        may_wait: bool,
        caller: Option<&Caller>,
    ) -> Result<(Workspace, Option<String>)> {
        let held = may_wait && self.hold_preparation(workspace, &start, caller).await?;
        let recorded = self.workspace_by_id(&workspace.id).await?;
        if held {
            return Ok((recorded, None));
        }
        let spec = OperationSpec::Prepare {
            workspace: SpaceSelector::id(&workspace.id),
            steps: start.steps,
        };
        let operation = self.start_operation(spec, start.env, caller).await?;
        Ok((recorded, Some(operation)))
    }

    /// Refuse the scoped `caller` the agent `preparing` queues in the
    /// `workspace` a start opened where it would start with a permission
    /// the caller does not hold: one it is given, or the value a key a
    /// reopened workspace's agent was given and this one leaves out falls
    /// back to.
    async fn ensure_start_agent_held(
        &self,
        workspace: &Workspace,
        preparing: &Preparing,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let (Some(caller), Some(agent)) = (caller, &preparing.agent) else {
            return Ok(());
        };
        let mode = (agent.mode.clone()).unwrap_or(crate::config::modes::ModeName::DEFAULT);
        let (giver, space) = (Some(&caller.space_id), SpaceSelector::id(&workspace.id));
        (self.ensure_agent_held(giver, &space, &mode, &agent.narrowing)).await
    }

    /// Assign `issue` to the `workspace` a start opened, as `caller`'s
    /// account on the forge, returning what that warns of. A new workspace
    /// whose issue cannot be assigned is removed, since nothing ran in it.
    async fn assign_start_issue(
        &self,
        workspace: &Workspace,
        reused: bool,
        issue: Option<OpenIssue>,
        caller: Option<&Caller>,
    ) -> Result<Vec<String>> {
        let Some(OpenIssue { url, title, .. }) = issue else {
            return Ok(Vec::new());
        };
        let selector = SpaceSelector::id(&workspace.id);
        let assigned = self
            .set_issue(&selector, &url.into(), Some(title), Assigner::of(caller))
            .await;
        if assigned.is_err() && !reused {
            self.discard_new(workspace).await;
        }
        assigned
    }

    /// Give the start's queue `entry` the `workspace` it opened, refusing
    /// the start when the workspace has an agent of its own the entry's
    /// launch did not wait for. Any other failure, such as an entry that no
    /// longer starts, leaves the start to go on without it.
    async fn give_start_entry(&self, entry: &QueueEntryId, workspace: &Workspace) -> Result<()> {
        match self
            .queue_attach(entry, &SpaceSelector::id(&workspace.id))
            .await
        {
            Err(error) if error.is::<HasAgent>() => Err(error),
            Err(error) => {
                log!(
                    "queue entry {entry} not given {}: {error:#}",
                    workspace.name
                );
                Ok(())
            }
            Ok(()) => Ok(()),
        }
    }

    /// Remove the new `workspace`, which nothing ran in yet.
    async fn discard_new(&self, workspace: &Workspace) {
        let removal = self
            .remove_selected(
                &SpaceSelector::id(&workspace.id),
                UnsavedWork::All,
                InspectionPolicy::GitOnly,
            )
            .await;
        if let Err(error) = removal {
            log!(
                "cannot remove {} after its creation failed: {error:#}",
                workspace.name
            );
        }
    }
}

#[cfg(test)]
mod tests;
