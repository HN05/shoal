//! The PRs a workspace watches until they merge, or the commit the user
//! acknowledged as merged.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use shoal_refs::CommitId;

use crate::forge::items::reference::{ItemInput, ItemRef};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RegistrationRecord", into = "RegistrationRecord")]
pub struct Registration {
    pub kind: RegistrationKind,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationKind {
    Watch {
        urls: Vec<ItemRef>,
        merged_head: Option<CommitId>,
    },
    /// Manual acknowledgement is tied to exactly this commit.
    Acknowledgement { head: CommitId },
}

/// Preserve legacy single-watch records while extending watch sets and completion.
#[derive(Serialize, Deserialize)]
struct RegistrationRecord {
    url: Option<ItemRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    urls: Option<Vec<ItemRef>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    merged_head: Option<CommitId>,
    head: Option<CommitId>,
    error: Option<String>,
}

impl TryFrom<RegistrationRecord> for Registration {
    type Error = anyhow::Error;

    fn try_from(record: RegistrationRecord) -> Result<Self> {
        let kind = match (record.url, record.urls, record.head, record.merged_head) {
            (Some(url), None, None, merged_head) => RegistrationKind::Watch {
                urls: vec![url],
                merged_head,
            },
            (None, Some(urls), None, merged_head) => {
                ensure!(
                    !urls.is_empty()
                        && urls.iter().collect::<std::collections::HashSet<_>>().len()
                            == urls.len(),
                    "invalid PR watches: expected nonempty, unique URLs"
                );
                RegistrationKind::Watch { urls, merged_head }
            }
            (None, None, Some(head), None) => RegistrationKind::Acknowledgement { head },
            _ => anyhow::bail!(
                "invalid PR cleanup registration: expected exactly one of url or head, or a watch URL list"
            ),
        };
        Ok(Self {
            kind,
            error: record.error,
        })
    }
}

impl From<Registration> for RegistrationRecord {
    fn from(registration: Registration) -> Self {
        let mut record = Self {
            url: None,
            urls: None,
            head: None,
            merged_head: None,
            error: registration.error,
        };
        match registration.kind {
            RegistrationKind::Watch {
                mut urls,
                merged_head,
            } => {
                if urls.len() == 1 {
                    record.url = urls.pop();
                } else {
                    record.urls = Some(urls);
                }
                record.merged_head = merged_head;
            }
            RegistrationKind::Acknowledgement { head } => record.head = Some(head),
        }
        record
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ActionFields", into = "ActionFields")]
pub enum Action {
    /// Watch the PR a number or URL names, recording its title when the
    /// caller knows it.
    Watch {
        url: ItemInput,
        title: Option<String>,
    },
    Unwatch {
        url: ItemInput,
    },
    Acknowledge,
    Clear,
}

/// The existing SetPr fields remain flattened on the wire.
#[derive(Serialize, Deserialize)]
struct ActionFields {
    url: Option<ItemInput>,
    clear: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unwatch: Option<ItemInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
}

impl TryFrom<ActionFields> for Action {
    type Error = anyhow::Error;

    fn try_from(fields: ActionFields) -> Result<Self> {
        let title = fields.title;
        ensure!(
            title.is_none() || fields.url.is_some(),
            "invalid PR cleanup action: only a watch has a title"
        );
        match (fields.url, fields.clear, fields.unwatch) {
            (Some(url), false, None) => Ok(Self::Watch { url, title }),
            (None, false, None) => Ok(Self::Acknowledge),
            (None, false, Some(url)) => Ok(Self::Unwatch { url }),
            (None, true, None) => Ok(Self::Clear),
            (Some(_), true, None) => {
                anyhow::bail!("invalid PR cleanup action: clear cannot include a URL")
            }
            _ => anyhow::bail!(
                "invalid PR cleanup action: unwatch cannot be combined with another action"
            ),
        }
    }
}

impl From<Action> for ActionFields {
    fn from(action: Action) -> Self {
        let (url, clear, unwatch, title) = match action {
            Action::Watch { url, title } => (Some(url), false, None, title),
            Action::Unwatch { url } => (None, false, Some(url), None),
            Action::Acknowledge => (None, false, None, None),
            Action::Clear => (None, true, None, None),
        };
        Self {
            url,
            clear,
            unwatch,
            title,
        }
    }
}
