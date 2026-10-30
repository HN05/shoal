//! The records a subscription covers, as they are now: those of the topics
//! it asked for that its caller may read.
use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;
use shoal_client::{
    Record, Topic,
    records::{RecordKey, RecordKind},
};

use super::{agents, records};
use crate::{
    daemon::{
        agent_messages::AgentMessage, approvals::DecisionStatus, notifications::NotificationKind,
        scope::Caller, store, workspace::Manager,
    },
    forge::updates::Snapshot,
    model::Workspace,
};

/// Records by kind and ID, in a stable order.
pub(super) type Records = BTreeMap<(String, String), Record>;

/// The topics a subscription asked for that this daemon serves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Topics(HashSet<&'static str>);

impl Topics {
    /// Every known topic of `topics`; notifications only for the user.
    pub(super) fn new(topics: &[Topic], caller: Option<&Caller>) -> Self {
        Self(
            topics
                .iter()
                .filter(|topic| caller.is_none() || **topic != Topic::Notifications)
                .filter_map(|topic| {
                    Topic::KNOWN
                        .iter()
                        .copied()
                        .find(|known| *known == topic.as_str())
                })
                .collect(),
        )
    }

    fn has(&self, topic: &Topic) -> bool {
        self.0.contains(topic.as_str())
    }
}

/// The records of `topics` that `caller` may read.
pub(super) async fn take(
    manager: &Manager,
    caller: Option<&Caller>,
    topics: &Topics,
) -> Result<Records> {
    let workspaces = visible_workspaces(manager, caller).await?;
    let mut found = Vec::new();
    if topics.has(&Topic::Repositories) {
        found.extend(repositories(manager, caller).await?);
    }
    if topics.has(&Topic::Workspaces) {
        found.extend(workspace_records(manager, &workspaces).await?);
    }
    if topics.has(&Topic::Links) {
        found.extend(links(manager, &workspaces).await?);
    }
    if topics.has(&Topic::Questions) {
        found.extend(questions(manager, &workspaces).await?);
    }
    if topics.has(&Topic::Approvals) {
        found.extend(approvals(manager, caller, &workspaces).await?);
    }
    if topics.has(&Topic::Messages) {
        found.extend(messages(manager, caller, &workspaces).await?);
    }
    if topics.has(&Topic::Notifications) {
        found.extend(notifications(manager).await?);
    }
    Ok(found
        .into_iter()
        .filter_map(|record| {
            let RecordKey { kind, id } = record.key()?;
            Some(((kind.to_string(), id), record))
        })
        .collect())
}

/// Every repository, or a scoped caller's own, as `repositories.list` lists them.
async fn repositories(manager: &Manager, caller: Option<&Caller>) -> Result<Vec<Record>> {
    Ok(manager
        .visible_repositories(caller)
        .await?
        .into_iter()
        .map(|repository| Record::Repository(records::repository(repository)))
        .collect())
}

async fn workspace_records(manager: &Manager, workspaces: &[Workspace]) -> Result<Vec<Record>> {
    let agents = agents::load(manager, workspaces).await?;
    Ok(workspaces
        .iter()
        .map(|workspace| Record::Workspace(Box::new(records::workspace(workspace, &agents))))
        .collect())
}

async fn links(manager: &Manager, workspaces: &[Workspace]) -> Result<Vec<Record>> {
    let mut activity = item_activity(manager).await?;
    Ok(workspaces
        .iter()
        .flat_map(|workspace| {
            let observed = activity.remove(&workspace.id).unwrap_or_default();
            records::links(workspace, &observed)
        })
        .map(|link| Record::Link(Box::new(link)))
        .collect())
}

async fn questions(manager: &Manager, workspaces: &[Workspace]) -> Result<Vec<Record>> {
    let visible = ids(workspaces);
    Ok(manager
        .open_questions()
        .await?
        .into_iter()
        .filter(|question| visible.contains(question.workspace_id.as_str()))
        .map(|question| Record::Question(records::question(question)))
        .collect())
}

/// Pending approval requests: all of them for the user, including issue
/// proposals that outlive their workspace, and those of its workspace tree
/// for a scoped caller.
async fn approvals(
    manager: &Manager,
    caller: Option<&Caller>,
    workspaces: &[Workspace],
) -> Result<Vec<Record>> {
    let visible = ids(workspaces);
    Ok(manager
        .approval_requests(None)
        .await?
        .into_iter()
        .filter(|request| {
            request.status == DecisionStatus::Pending
                && (caller.is_none() || visible.contains(request.workspace_id.as_str()))
        })
        .map(|request| Record::Approval(records::approval(request)))
        .collect())
}

/// Messages queued for the agents of `workspaces`, and for the user, who
/// alone reads the messages agents send it.
async fn messages(
    manager: &Manager,
    caller: Option<&Caller>,
    workspaces: &[Workspace],
) -> Result<Vec<Record>> {
    let mut messages: Vec<Record> = agent_messages(manager)
        .await?
        .into_iter()
        .filter_map(|(workspace_id, message)| {
            let workspace = workspaces.iter().find(|w| w.id == workspace_id)?;
            Some(Record::Message(records::agent_message(message, workspace)))
        })
        .collect();
    if caller.is_none() {
        messages.extend(
            manager
                .notifications(false, u32::MAX)
                .await?
                .iter()
                .filter(|notification| notification.kind == NotificationKind::AgentMessage)
                .map(|notification| Record::Message(records::user_message(notification))),
        );
    }
    Ok(messages)
}

async fn notifications(manager: &Manager) -> Result<Vec<Record>> {
    Ok(manager
        .notifications(false, u32::MAX)
        .await?
        .into_iter()
        .map(|notification| Record::Notification(records::notification(notification)))
        .collect())
}

fn ids(workspaces: &[Workspace]) -> HashSet<&str> {
    workspaces
        .iter()
        .map(|workspace| workspace.id.as_str())
        .collect()
}

/// The record `kind` and `id` name in `records`.
pub(super) fn key(kind: &str, id: &str) -> RecordKey {
    RecordKey {
        kind: RecordKind::from(kind),
        id: id.to_owned(),
    }
}

/// Every workspace, or a scoped caller's own and those split from it.
async fn visible_workspaces(manager: &Manager, caller: Option<&Caller>) -> Result<Vec<Workspace>> {
    let mut workspaces = manager.list_workspaces().await?;
    if let Some(caller) = caller {
        let subtree = manager.subtree(&caller.workspace_id).await?;
        workspaces.retain(|workspace| subtree.contains(&workspace.id));
    }
    Ok(workspaces)
}

/// What the daemon last observed of each linked item on its forge, by
/// workspace ID and URL.
async fn item_activity(manager: &Manager) -> Result<HashMap<String, HashMap<String, Snapshot>>> {
    let rows = manager
        .store
        .run(|db| {
            Ok(db
                .prepare("SELECT workspace_id,url,record FROM workspace_item_activity")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<rusqlite::Result<Vec<(String, String, String)>>>()?)
        })
        .await?;
    let mut activity: HashMap<String, HashMap<String, Snapshot>> = HashMap::new();
    for (workspace, url, record) in rows {
        // An unreadable observation shows the link without one.
        if let Ok(snapshot) = serde_json::from_str(&record) {
            activity.entry(workspace).or_default().insert(url, snapshot);
        }
    }
    Ok(activity)
}

/// Every message queued for a workspace's agent, with its workspace's ID.
async fn agent_messages(manager: &Manager) -> Result<Vec<(String, AgentMessage)>> {
    manager
        .store
        .run(|db| {
            let mut statement = db.prepare(
                "SELECT workspace_id,id,created_at,message,attachments FROM agent_messages ORDER BY id",
            )?;
            let mut rows = statement.query([])?;
            let mut messages = Vec::new();
            while let Some(row) = rows.next()? {
                messages.push((
                    row.get("workspace_id")?,
                    AgentMessage {
                        id: row.get("id")?,
                        created_at: row.get("created_at")?,
                        message: row.get("message")?,
                        attachments: store::json_column(row, "attachments")?.unwrap_or_default(),
                    },
                ));
            }
            Ok(messages)
        })
        .await
}
