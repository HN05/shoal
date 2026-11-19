//! The plan of a queued agent start, which everything that queues one and
//! the `queue-start` worker that carries it out agree on. The queue keeps it
//! as JSON in [`QueueRequest::plan`] until the worker runs; the protocol
//! calls live in `shoal-client`'s `queue`, the worker in `shoal-workers`'
//! `queued_starts` and the daemon's runner in `daemon::queued_starts`.
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use std::{ffi::OsString, path::PathBuf};

use anyhow::Result;

use crate::{
    agent::{Agent, ModelChoice},
    model::{QueueRequest, Workspace},
    protocol::{
        CreationOptions, PullHead,
        operations::{QueuedLaunch, WorkspaceStart},
    },
    terminal::PlaceName,
};
use shoal_protocol::space_names::SpaceSelector;

mod place;
pub use place::open_add;

/// The plan of a queued agent start, the same for every kind of space: the
/// launch of the agent of the recorded space its entry names, and the PR it
/// links as it starts.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QueuedStart {
    pub launch: QueuedLaunch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<crate::forge::items::reference::ItemInput>,
}

impl QueuedStart {
    /// The plan a queue entry holds as the JSON `plan`.
    pub fn of(plan: &str) -> Result<Self> {
        anyhow::Context::context(serde_json::from_str(plan), "read the queued launch's plan")
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct Creation {
    pub path: Option<PathBuf>,
    pub branch: Option<String>,
    pub existing: Option<String>,
    pub base: Option<String>,
    pub git_profile: Option<String>,
    /// The PR whose head branch is opened; it is linked once the workspace is ready.
    #[serde(default)]
    pub pr: Option<crate::forge::items::reference::ItemInput>,
    #[serde(default)]
    pub options: CreationOptions,
    /// The head of a PR from a fork, opened in a workspace at a commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_head: Option<PullHead>,
    /// The workspace the new one copies, on `branch` or a branch named
    /// after the copied one's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy: Option<CopyCreation>,
    /// `branch` is the user's own name, which the new branch takes exactly
    /// or is refused, rather than a name Shoal derived and may suffix.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub typed: bool,
}

/// The workspace a new one copies, and the name the copy takes without a
/// branch of its own, which the daemon suffixes when it is taken.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct CopyCreation {
    pub source: crate::protocol::CopySource,
    pub name: String,
}

impl Creation {
    /// Whether nothing names the branch yet: no new or existing branch, PR
    /// head or copy.
    pub fn unsettled(&self) -> bool {
        self.branch.is_none()
            && self.existing.is_none()
            && self.pull_head.is_none()
            && self.copy.is_none()
    }
}

/// What opening a workspace a [`Creation`] describes starts at, with the
/// rest it opens with.
pub struct Opening {
    pub start: WorkspaceStart,
    pub base: Option<String>,
    pub path: Option<PathBuf>,
    pub git_profile: Option<String>,
    pub options: CreationOptions,
    /// The PR it links.
    pub pr: Option<crate::forge::items::reference::ItemInput>,
}

impl Creation {
    /// What opening this creation starts at: a PR's head, an existing
    /// branch, a copy, or a new branch, typed or named.
    pub fn opening(self) -> Result<Opening> {
        let Self {
            path,
            branch,
            existing,
            base,
            git_profile,
            pr,
            options,
            pull_head,
            copy,
            typed,
        } = self;
        let start = match (pull_head, existing, copy) {
            (Some(head), ..) => WorkspaceStart::PullHead {
                url: match &pr {
                    Some(crate::forge::items::reference::ItemInput::Url(url)) => url.clone(),
                    _ => anyhow::bail!("a PR head needs its PR's URL"),
                },
                head,
            },
            (None, Some(branch), _) => WorkspaceStart::Branch(branch),
            (None, None, Some(copy)) => WorkspaceStart::Copy {
                source: copy.source,
                branch,
                typed,
            },
            (None, None, None) => {
                let branch = anyhow::Context::context(branch, "a branch name is required")?;
                match typed {
                    true => WorkspaceStart::Typed(branch),
                    false => WorkspaceStart::Name(branch),
                }
            }
        };
        Ok(Opening {
            start,
            base,
            path,
            git_profile,
            options,
            pr,
        })
    }
}

/// The agent of a plan, which queued entries store; entries queued before
/// every launch ran tracked also carry `tracked` or `codex_mode`, which are
/// ignored.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ResolvedAddAgent {
    pub agent: Option<Agent>,
    /// The model and effort requested over the configured ones.
    #[serde(default)]
    pub model: ModelChoice,
}

/// The queue request of `launch` in the recorded `space`, a workspace or a
/// chat, which the agent queue starts detached, beside the caller's
/// terminal when the launch names it or else in the default host, or
/// leaves to a caller that runs it `here`, linking `pr` as it starts.
pub fn queued_agent_request<P>(
    space: &crate::model::Space<P>,
    mut launch: QueuedLaunch,
    pr: Option<crate::forge::items::reference::ItemInput>,
) -> Result<QueueRequest> {
    let env =
        (launch.env.take()).unwrap_or_else(|| crate::terminal::detached_start_environment(None));
    let issue_title = launch.issue.as_ref().map(|issue| issue.title.clone());
    let issue_labels = (launch.issue.as_ref()).map_or_else(Vec::new, |issue| issue.labels.clone());
    let (agent, now, here) = (launch.agent.clone(), launch.now, launch.here);
    let (mode, narrowing) = (launch.mode.clone(), launch.narrowing);
    let source = launch.source.clone();
    let plan = QueuedStart { launch, pr };
    Ok(QueueRequest {
        name: space.name.clone(),
        agent,
        mode,
        narrowing,
        issue_title,
        issue_labels,
        plan: serde_json::to_string(&plan)?,
        env,
        now,
        space: SpaceSelector::id(&space.id),
        starts_in_caller: here,
        blockers: Vec::new(),
        source,
    })
}

/// What the place a start opens beside its terminal hands over to: the
/// operation preparing its workspace, which it follows first, if any.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PlaceHandoff {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation: Option<String>,
}

/// The agent a start runs in a recorded workspace, or the workspace's shell
/// without one, which a terminal host or the agent queue's worker carries
/// out.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct AddPlan {
    pub repository: crate::repository::Selector,
    /// The workspace the start opens and runs its agent in.
    pub workspace: SpaceId,
    /// The PR it links once the workspace is ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<crate::forge::items::reference::ItemInput>,
    /// The name of the place a host opens for the start.
    #[serde(default)]
    pub place_name: Option<PlaceName>,
    /// The found issue's URL; its body can exceed what the environment of
    /// the place a host opens carries, so the worker looks it up again.
    pub issue: Option<crate::forge::items::reference::ItemRef>,
    pub agent: ResolvedAddAgent,
    pub args: Vec<OsString>,
    /// The agent's subject when no issue gives it one, which a launcher
    /// with `{prompt}` receives there.
    #[serde(default)]
    pub prompt: Option<String>,
    /// The mode the agent starts in; the default mode when `None`.
    #[serde(default)]
    pub mode: Option<crate::config::modes::ModeName>,
    /// The permissions `--permission` gives its agent, whatever its mode's.
    #[serde(
        default,
        skip_serializing_if = "crate::config::permissions::PermissionsConfig::is_empty"
    )]
    pub narrowing: crate::config::permissions::PermissionsConfig,
    /// The session the agent starts from a fork of.
    #[serde(default)]
    pub fork: Option<shoal_ids::agent_session_ids::AgentSessionId>,
    /// The agent queue entry whose slot this launch uses.
    #[serde(default)]
    pub queue_entry: Option<shoal_ids::queue_entry_ids::QueueEntryId>,
    /// The recorded launch the place a start opens beside its terminal
    /// carries out: it follows the preparation the handoff names, if any,
    /// then starts the launch's queued agent there, or opens a shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<PlaceHandoff>,
}

impl AddPlan {
    /// `launch` in the existing `workspace`. It runs detached, so it stays
    /// tracked.
    pub fn agent_in(workspace: &Workspace, launch: QueuedLaunch) -> Result<Self> {
        let QueuedLaunch {
            agent,
            prompt,
            mode,
            narrowing,
            model,
            args,
            fork,
            issue,
            env: _,
            now: _,
            here: _,
            source: _,
        } = launch;
        Ok(Self {
            issue: issue.map(|issue| issue.url),
            agent: ResolvedAddAgent {
                agent: Some(agent.name().parse()?),
                model,
            },
            args: args.into_iter().map(OsString::from).collect(),
            prompt,
            mode,
            narrowing,
            fork,
            ..Self::opening(workspace)
        })
    }

    /// The plan that runs the queued `start` in `workspace`, the space its
    /// entry names, taking the slot of `entry`.
    pub fn queued(workspace: &Workspace, start: QueuedStart, entry: QueueEntryId) -> Result<Self> {
        let QueuedStart { launch, pr } = start;
        Ok(Self {
            pr,
            queue_entry: Some(entry),
            ..Self::agent_in(workspace, launch)?
        })
    }

    /// The plan that opens the existing `workspace` without an agent.
    pub fn opening(workspace: &Workspace) -> Self {
        Self {
            repository: workspace.repository().into(),
            workspace: workspace.id.clone(),
            pr: None,
            place_name: None,
            issue: None,
            agent: ResolvedAddAgent {
                agent: None,
                model: ModelChoice::default(),
            },
            args: Vec::new(),
            prompt: None,
            mode: None,
            narrowing: Default::default(),
            fork: None,
            queue_entry: None,
            handoff: None,
        }
    }

    /// Whether the plan opens `workspace`, the one it names in its
    /// repository.
    pub fn opens(&self, workspace: &Workspace) -> bool {
        self.workspace == workspace.id && self.repository.value() == *workspace.repository()
    }

    /// The agent's name, when the addition starts one.
    pub fn agent_name(&self) -> Option<Agent> {
        self.agent.agent.clone()
    }

    /// The found issue.
    pub fn issue(&self) -> Option<&crate::forge::items::reference::ItemRef> {
        self.issue.as_ref()
    }

    /// The mode the agent starts in.
    pub fn mode(&self) -> crate::config::modes::ModeName {
        (self.mode.clone()).unwrap_or(crate::config::modes::ModeName::DEFAULT)
    }

    /// Whether the agent starts with input: an issue prompt or forwarded arguments.
    /// A workspace shell or an agent without input waits for the user.
    pub fn runs_unattended(&self) -> bool {
        self.agent.agent.is_some()
            && (self.issue.is_some() || self.prompt.is_some() || !self.args.is_empty())
    }

    /// What the place a host opens for the start in `workspace` shows.
    pub fn label(&self, workspace: &Workspace) -> String {
        let name = workspace.name.as_str();
        (self.place_name.as_ref()).map_or_else(
            || name.to_owned(),
            |place| place.render(workspace.branch_or_name(), name),
        )
    }
}

#[cfg(test)]
mod tests;
