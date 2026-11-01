//! The PRs a workspace watches until they merge, or the commit the user
//! acknowledged as merged.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RegistrationRecord", into = "RegistrationRecord")]
pub struct Registration {
    pub kind: RegistrationKind,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationKind {
    Watch {
        urls: Vec<String>,
        merged_head: Option<String>,
    },
    /// Manual acknowledgement is tied to exactly this commit.
    Acknowledgement { head: String },
}

/// Preserve legacy single-watch records while extending watch sets and completion.
#[derive(Serialize, Deserialize)]
struct RegistrationRecord {
    url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    urls: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    merged_head: Option<String>,
    head: Option<String>,
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
                        && urls.iter().all(|url| !url.is_empty())
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
    Watch { url: String },
    Unwatch { url: String },
    Acknowledge,
    Clear,
}

/// The existing SetPr fields remain flattened on the wire.
#[derive(Serialize, Deserialize)]
struct ActionFields {
    url: Option<String>,
    clear: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unwatch: Option<String>,
}

impl TryFrom<ActionFields> for Action {
    type Error = anyhow::Error;

    fn try_from(fields: ActionFields) -> Result<Self> {
        match (fields.url, fields.clear, fields.unwatch) {
            (Some(url), false, None) => Ok(Self::Watch { url }),
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
        let (url, clear, unwatch) = match action {
            Action::Watch { url } => (Some(url), false, None),
            Action::Unwatch { url } => (None, false, Some(url)),
            Action::Acknowledge => (None, false, None),
            Action::Clear => (None, true, None),
        };
        Self {
            url,
            clear,
            unwatch,
        }
    }
}
