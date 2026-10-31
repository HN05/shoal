use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};

use super::Lifetime;
use crate::forge::download::Download;

/// Requests about the issues and PRs of a workspace's repository and the ones it links.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemMethod {
    SetIssue {
        workspace: String,
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// Link an issue or PR by number or URL with a relation.
    LinkItem {
        workspace: String,
        kind: crate::forge::items::link::LinkKind,
        url: String,
        relation: crate::model::Relation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// Unlink the relation's items, optionally only one kind or one item.
    UnlinkItems {
        workspace: String,
        relation: crate::model::Relation,
        kind: Option<crate::forge::items::link::LinkKind>,
        url: Option<String>,
    },
    SetPr {
        workspace: String,
        #[serde(flatten)]
        action: crate::daemon::links::pr::Action,
    },
    PrWait {
        workspace: String,
        timeout_secs: Option<u64>,
    },
    WatchItems {
        workspace: String,
        selection: crate::forge::items::link::Selection,
        timeout_secs: Option<u64>,
    },
    /// Link the open PR of the workspace's branch, or open one and link it.
    OpenPull {
        workspace: String,
        options: crate::forge::items::create::PullOptions,
    },
    /// Have the forge open the revert of a merged PR: the one `item` names,
    /// or the workspace's linked one.
    RevertPull {
        workspace: String,
        item: Option<String>,
        options: crate::forge::items::create::PullOptions,
    },
    /// A merged PR with what reverting its merge with Git needs.
    MergedPull {
        workspace: String,
        item: Option<String>,
    },
    /// Open an issue in the context's repository, or the one `repository`
    /// names, linking it to the workspace when `link` is set.
    OpenIssue {
        #[serde(flatten)]
        context: ItemContext,
        issue: crate::forge::items::create::NewIssue,
        repository: Option<String>,
        link: bool,
    },
    /// Change an issue or PR of the context's repository: the one `item`
    /// names, or the workspace's linked one of `kind`.
    ItemAction {
        #[serde(flatten)]
        context: ItemContext,
        kind: crate::forge::items::link::ItemKind,
        item: Option<String>,
        action: crate::forge::items::action::Action,
    },
    /// Issues or PRs of the context's repository, or the one `repository`
    /// names, that `search` matches, answered in one or more `items` frames
    /// and then `ok`.
    ListItems {
        #[serde(flatten)]
        context: ItemContext,
        kind: crate::forge::items::link::ItemKind,
        search: crate::forge::items::list::Search,
        repository: Option<String>,
    },
    /// A request to the forge API of the context's repository, or the one
    /// `repository` names, answered with the response's JSON text in one or
    /// more `api_response` frames and then `ok`.
    ForgeApi {
        #[serde(flatten)]
        context: ItemContext,
        request: crate::forge::raw::RawRequest,
        repository: Option<String>,
    },
    /// A file on the forge of the context's repository, or of the one
    /// `repository` names, answered with its content type and bytes in one
    /// or more `downloaded` frames and then `ok`.
    ForgeDownload {
        #[serde(flatten)]
        context: ItemContext,
        url: String,
        repository: Option<String>,
    },
    /// Linked items, or an explicit one, for the CLI to look up; with
    /// `elsewhere`, also an issue or PR of another repository by its URL.
    SelectItems {
        workspace: String,
        selection: crate::forge::items::link::Selection,
        #[serde(default)]
        elsewhere: bool,
    },
    /// The forge `host` serves, asking it over `scheme` once when neither
    /// the global config nor a recorded answer says.
    ForgeType { host: String, scheme: String },
}

/// A piece of a downloaded file, sent in order: its content type, if the
/// forge named one, then its bytes in base64 pieces that each fit a frame.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadPiece {
    ContentType(String),
    Bytes(String),
}

impl DownloadPiece {
    /// The pieces that carry `download`.
    pub fn split(download: Download) -> Vec<Self> {
        const PIECE_BYTES: usize = super::MAX_FRAME / 4 * 3 / 2;
        let content_type = download.content_type.map(Self::ContentType);
        let bytes = download
            .bytes
            .chunks(PIECE_BYTES)
            .map(|chunk| Self::Bytes(BASE64.encode(chunk)));
        content_type.into_iter().chain(bytes).collect()
    }

    /// The download `pieces` carry.
    pub fn join(pieces: Vec<Self>) -> Result<Download> {
        let mut download = Download {
            content_type: None,
            bytes: Vec::new(),
        };
        for piece in pieces {
            match piece {
                Self::ContentType(content_type) => download.content_type = Some(content_type),
                Self::Bytes(bytes) => download.bytes.extend(
                    BASE64
                        .decode(bytes)
                        .context("the daemon sent an invalid file piece")?,
                ),
            }
        }
        Ok(download)
    }
}

/// Where an item request runs: in a workspace, or in the registered checkout
/// of a repository, named by its ID, for a request that needs only the
/// repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemContext {
    Workspace(String),
    Checkout(String),
}

impl ItemMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::PrWait { .. } | Self::WatchItems { .. } => Lifetime::Resumed,
            _ => Lifetime::Reply,
        }
    }
}
