//! How an agent start is queued and placed: the queue request that
//! everything that queues one builds, which keeps its start in
//! [`QueueRequest::start`] until the `queue-start` worker carries it out,
//! and what the place the worker opens runs.
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use std::path::PathBuf;

use anyhow::Result;

use crate::{
    model::{QueueRequest, QueuedStart},
    protocol::{
        CreationOptions, PullHead,
        operations::{QueuedLaunch, WorkspaceStart},
    },
    terminal::PlaceName,
};
use shoal_protocol::space_names::SpaceSelector;

mod place;
pub use place::open_place;

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
    /// The commit keeping the unsaved files a new branch at `base` starts
    /// with, as a copy another joined machine carried here does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<shoal_refs::CommitId>,
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
    pub pr: Option<crate::forge::items::reference::ItemInput>,
    /// The commit keeping the unsaved files its new branch starts with.
    pub files: Option<shoal_refs::CommitId>,
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
            files,
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
            files,
        })
    }
}

/// The queue request of `launch` in the recorded `space`, a workspace or a
/// chat, which the agent queue starts detached, beside the caller's
/// terminal when the launch names it or else in the default host, or
/// leaves to a caller that runs it `here`, linking `pr` as it starts.
pub fn queued_agent_request<P>(
    space: &crate::model::Space<P>,
    mut launch: QueuedLaunch,
    pr: Option<crate::forge::items::reference::ItemInput>,
) -> QueueRequest {
    let env =
        (launch.env.take()).unwrap_or_else(|| crate::terminal::detached_start_environment(None));
    QueueRequest {
        name: space.name.clone(),
        start: QueuedStart { launch, pr },
        env,
        space: SpaceSelector::id(&space.id),
        blockers: Vec::new(),
    }
}

/// What the place a start opens beside its terminal hands over to: the
/// operation preparing its workspace, which it follows first, if any.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PlaceHandoff {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation: Option<String>,
}

/// What runs in a place opened for a recorded space, a workspace or a
/// chat: the agent of its starting launch, or a workspace's shell without
/// one, which a terminal host or the agent queue's worker carries out.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct PlacePlan {
    /// The space whose agent, or shell, runs in the place.
    pub space: SpaceId,
    /// The name of the place a host opens for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place_name: Option<PlaceName>,
    /// The starting launch whose agent runs there, taking its slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_entry: Option<QueueEntryId>,
    /// The start the place carries out for a caller that holds its launch:
    /// it follows the preparation the handoff names, if any, then starts
    /// the launch's agent once the queue lets it, or opens a shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<PlaceHandoff>,
}

impl PlacePlan {
    /// The plan that runs the agent of the starting launch `entry` in
    /// `space`, or opens a workspace's shell without one.
    pub fn new(space: &SpaceId, entry: Option<&QueueEntryId>) -> Self {
        Self {
            space: space.clone(),
            place_name: None,
            queue_entry: entry.cloned(),
            handoff: None,
        }
    }

    /// What the place a host opens for the start in `space` shows: its
    /// templated name in a workspace, else the space's name.
    pub(crate) fn label(&self, space: &crate::model::Space) -> String {
        match (&self.place_name, space.to_workspace()) {
            (Some(place), Some(workspace)) => {
                place.render(workspace.branch_or_name(), &workspace.name)
            }
            _ => space.name.to_string(),
        }
    }
}

#[cfg(test)]
mod tests;
