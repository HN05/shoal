//! A launch that starts its agent in a new space: the mode's kind decides
//! where it goes, a new chat in a checkout, a worktree or `root_dir`, or a
//! new workspace, which its preparation readies, or, where a workspace's
//! launch names a branch an existing workspace has, that workspace's own
//! agent.
use std::sync::Arc;

use anyhow::Result;
use shoal_protocol::launches::{Launch, LaunchSource, Launched};

use super::shape::Shape;
use crate::{
    daemon::{scope::Caller, workspace::Manager},
    model::SpaceKind,
};

impl Manager {
    /// Start the agent of `launch`, which `source` made, in the new space
    /// `shape` says, for `caller`.
    pub(super) async fn launch_space(
        self: &Arc<Self>,
        launch: Launch,
        shape: Shape,
        source: LaunchSource,
        caller: Option<&Caller>,
    ) -> Result<Launched> {
        match shape.kind {
            SpaceKind::Chat => self.launch_new_chat(launch, shape, source, caller).await,
            SpaceKind::Workspace => self.launch_workspace(launch, shape, source, caller).await,
        }
    }
}
