use crate::{attaching::Terminal, space_names::SpaceSelector};
use serde::{Deserialize, Serialize};

use super::{Forwarding, Lifetime, Target};

/// Requests about workspaces' running demos.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DemoMethod {
    /// Running demos, of one workspace or, unscoped, of every workspace.
    ListDemos { workspace: Option<SpaceSelector> },
    /// How far the start of one demo launch in a workspace got.
    DemoProgress {
        workspace: SpaceSelector,
        launch: String,
    },
    /// Ask a workspace's demos to stop: the one named, or every one.
    StopDemos {
        workspace: SpaceSelector,
        name: Option<String>,
    },
    /// Open a stream to a workspace's running demo `name`, on whichever
    /// machine it runs: answered with [`super::Body::DemoStream`], after
    /// which the connection carries the demo's bytes both ways until either
    /// side closes it. A `server` or `web` demo's are those of one TCP
    /// connection to its port. A terminal demo's are the frames of
    /// [`crate::attaching`]: what a terminal made like `terminal` shows of
    /// the demo's place one way, the viewer's keys and sizes the other.
    OpenDemoStream {
        workspace: SpaceSelector,
        name: String,
        #[serde(default)]
        terminal: Terminal,
    },
}

impl DemoMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::OpenDemoStream { .. } => Lifetime::Open,
            _ => Lifetime::Reply,
        }
    }
}

impl DemoMethod {
    /// A demo runs on its workspace's machine, whose stream carries its
    /// bytes both ways; a start's progress is that of a start the caller
    /// made.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::ListDemos { .. } | Self::StopDemos { .. } => Forwarding::Online,
            Self::OpenDemoStream { .. } => Forwarding::Duplex,
            Self::DemoProgress { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::ListDemos {
                workspace: Some(workspace),
            }
            | Self::DemoProgress { workspace, .. }
            | Self::StopDemos { workspace, .. }
            | Self::OpenDemoStream { workspace, .. } => Some(Target::Space(workspace)),
            Self::ListDemos { workspace: None } => None,
        }
    }
}
