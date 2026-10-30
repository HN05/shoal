use serde::{Deserialize, Serialize};

use super::Lifetime;

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
    /// A request to the forge API of the workspace's repository, or the one
    /// `repository` names, answered with the response's JSON text in one or
    /// more `api_response` frames and then `ok`.
    ForgeApi {
        workspace: String,
        request: crate::forge::raw::RawRequest,
        repository: Option<String>,
    },
    /// Linked items, or an explicit one, for the CLI to look up.
    SelectItems {
        workspace: String,
        selection: crate::forge::items::link::Selection,
    },
    /// The forge `host` serves, asking it over `scheme` once when neither
    /// the global config nor a recorded answer says.
    ForgeType { host: String, scheme: String },
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
