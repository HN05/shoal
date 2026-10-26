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
        kind: crate::forge::items::link::ItemKind,
        url: String,
        relation: crate::model::Relation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// Unlink the relation's items, optionally only one kind or one item.
    UnlinkItems {
        workspace: String,
        relation: crate::model::Relation,
        kind: Option<crate::forge::items::link::ItemKind>,
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
    /// Open an issue in the workspace's repository, or the one `repository`
    /// names, linking it when `link` is set.
    OpenIssue {
        workspace: String,
        issue: crate::forge::items::create::NewIssue,
        repository: Option<String>,
        link: bool,
    },
    /// Change an issue or PR of the workspace's repository: the one `item`
    /// names, or the workspace's linked one of `kind`.
    ItemAction {
        workspace: String,
        kind: crate::forge::items::link::ItemKind,
        item: Option<String>,
        action: crate::forge::items::action::Action,
    },
    /// Issues or PRs of the workspace's repository that `search` matches,
    /// answered in one or more `items` frames and then `ok`.
    ListItems {
        workspace: String,
        kind: crate::forge::items::link::ItemKind,
        search: crate::forge::items::list::Search,
    },
    /// A request to the workspace repository's forge API, answered with
    /// the response's JSON text in one or more `api_response` frames and
    /// then `ok`.
    ForgeApi {
        workspace: String,
        request: crate::forge::raw::RawRequest,
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

impl ItemMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::PrWait { .. } | Self::WatchItems { .. } => Lifetime::Resumed,
            _ => Lifetime::Reply,
        }
    }
}
