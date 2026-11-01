//! The records a subscription covers, as they are now: those of the topics
//! it asked for that its caller may read.
use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;

use super::agents;
use crate::{
    client::subscriptions::records::{
        Check, Link, LookupError, Message, MessageRecipient, Record, RecordKey, Topic,
        WorkspaceRecord,
    },
    daemon::{
        agent_messages::AgentMessage,
        approvals::DecisionStatus,
        notifications::{Notification, NotificationKind},
        scope::Caller,
        store,
        workspace::Manager,
    },
    forge::{items::link::LinkKind, updates::Snapshot},
    model::{Chat, Relation, Workspace},
};

/// Records by key, each with its JSON, which tells a changed one apart.
pub(super) type Records = BTreeMap<RecordKey, (Record, serde_json::Value)>;

/// The topics a subscription covers: those it asked for, without
/// notifications for a scoped caller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Topics(HashSet<Topic>);

impl Topics {
    pub(super) fn new(topics: &[Topic], caller: Option<&Caller>) -> Self {
        Self(
            topics
                .iter()
                .copied()
                .filter(|topic| caller.is_none() || *topic != Topic::Notifications)
                .collect(),
        )
    }

    fn has(&self, topic: Topic) -> bool {
        self.0.contains(&topic)
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
    if topics.has(Topic::Repositories) {
        found.extend(repositories(manager, caller).await?);
    }
    if topics.has(Topic::Workspaces) {
        found.extend(workspace_records(manager, &workspaces).await?);
    }
    if topics.has(Topic::Links) {
        found.extend(links(manager, &workspaces).await?);
    }
    if topics.has(Topic::Questions) {
        let chats = manager.visible_chats(caller).await?;
        found.extend(questions(manager, &workspaces, &chats).await?);
    }
    if topics.has(Topic::Approvals) {
        found.extend(approvals(manager, caller, &workspaces).await?);
    }
    if topics.has(Topic::Messages) {
        found.extend(messages(manager, caller, &workspaces).await?);
    }
    if topics.has(Topic::Notifications) {
        found.extend(notifications(manager).await?);
    }
    let mut records = Records::new();
    for record in found {
        let value = serde_json::to_value(&record)?;
        records.insert(record.key(), (record, value));
    }
    Ok(records)
}

/// Every repository, or a scoped caller's own, as listing them shows.
async fn repositories(manager: &Manager, caller: Option<&Caller>) -> Result<Vec<Record>> {
    Ok(manager
        .visible_repositories(caller)
        .await?
        .into_iter()
        .map(Record::Repository)
        .collect())
}

async fn workspace_records(manager: &Manager, workspaces: &[Workspace]) -> Result<Vec<Record>> {
    let agents = agents::load(manager, workspaces).await?;
    Ok(workspaces
        .iter()
        .map(|workspace| {
            Record::Workspace(Box::new(WorkspaceRecord {
                agent: agents.activity(workspace),
                workspace: workspace.clone(),
            }))
        })
        .collect())
}

async fn links(manager: &Manager, workspaces: &[Workspace]) -> Result<Vec<Record>> {
    let mut activity = item_activity(manager).await?;
    Ok(workspaces
        .iter()
        .flat_map(|workspace| {
            let observed = activity.remove(&workspace.id).unwrap_or_default();
            workspace_links(workspace, &observed)
        })
        .map(|link| Record::Link(Box::new(link)))
        .collect())
}

/// Each item `workspace` links, with what the daemon last observed of it on
/// its forge, from `activity` by URL.
fn workspace_links(workspace: &Workspace, activity: &HashMap<String, Snapshot>) -> Vec<Link> {
    let links = &workspace.links;
    let assigned = links
        .issue
        .iter()
        .map(|url| (LinkKind::Issue, url, links.issue_title.clone()))
        .chain(links.prs.iter().map(|url| (LinkKind::Pr, url, None)))
        .map(|(kind, url, title)| (Relation::Assigned, kind, url, title));
    let recorded = [
        (Relation::Opened, &links.opened),
        (Relation::Related, &links.related),
    ]
    .into_iter()
    .flat_map(|(relation, items)| {
        items
            .iter()
            .map(move |item| (relation, item.kind, &item.url, item.title.clone()))
    });
    assigned
        .chain(recorded)
        .map(|(relation, kind, url, title)| {
            let observed = activity.get(url);
            let present = |value: &str| (!value.is_empty()).then(|| value.to_owned());
            Link {
                id: format!("{}/{relation}/{url}", workspace.id),
                workspace_id: workspace.id.clone(),
                kind,
                url: url.clone(),
                relation,
                title,
                state: observed.and_then(|observed| present(observed.state())),
                head: observed.and_then(|observed| present(observed.head())),
                checks: observed.map_or_else(Vec::new, checks),
                conflict: observed.and_then(Snapshot::head_conflict),
                lookup_errors: observed.map_or_else(Vec::new, lookup_errors),
            }
        })
        .collect()
}

/// The CI checks of a PR's head, as the daemon last observed them.
fn checks(observed: &Snapshot) -> Vec<Check> {
    observed
        .head_checks()
        .map(|(name, state, complete)| Check {
            name: name.into(),
            state: state.into(),
            complete,
        })
        .collect()
}

/// What the daemon could not read of an item when it last looked.
fn lookup_errors(observed: &Snapshot) -> Vec<LookupError> {
    observed
        .errors()
        .map(|(source, message)| LookupError {
            source: source.into(),
            message: message.into(),
        })
        .collect()
}

/// The open questions of the visible workspaces and chats.
async fn questions(
    manager: &Manager,
    workspaces: &[Workspace],
    chats: &[Chat],
) -> Result<Vec<Record>> {
    let mut visible = ids(workspaces);
    visible.extend(chats.iter().map(|chat| chat.id.as_str()));
    Ok(manager
        .open_questions()
        .await?
        .into_iter()
        .filter(|question| visible.contains(question.space_id.as_str()))
        .map(Record::Question)
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
        .map(|request| Record::Approval(Box::new(request)))
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
            Some(Record::Message(agent_message(message, workspace)))
        })
        .collect();
    if caller.is_none() {
        messages.extend(
            manager
                .notifications(false, u32::MAX)
                .await?
                .iter()
                .filter(|notification| notification.kind == NotificationKind::AgentMessage)
                .map(|notification| Record::Message(user_message(notification))),
        );
    }
    Ok(messages)
}

/// A message queued for the agent of `workspace`.
fn agent_message(message: AgentMessage, workspace: &Workspace) -> Message {
    Message {
        id: format!("agent/{}", message.id),
        recipient: MessageRecipient::Agent,
        workspace_id: Some(workspace.id.clone()),
        workspace: Some(workspace.name.clone()),
        text: message.message,
        attachments: message.attachments,
        created_at: message.created_at,
    }
}

/// A workspace process's message to the user, which its notification records.
fn user_message(notification: &Notification) -> Message {
    Message {
        id: format!("user/{}", notification.id),
        recipient: MessageRecipient::User,
        workspace_id: None,
        workspace: notification.workspace.clone(),
        text: notification.message.clone(),
        attachments: Vec::new(),
        created_at: notification.created_at,
    }
}

async fn notifications(manager: &Manager) -> Result<Vec<Record>> {
    Ok(manager
        .notifications(false, u32::MAX)
        .await?
        .into_iter()
        .map(Record::Notification)
        .collect())
}

fn ids(workspaces: &[Workspace]) -> HashSet<&str> {
    workspaces
        .iter()
        .map(|workspace| workspace.id.as_str())
        .collect()
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
                .prepare("SELECT space_id,url,record FROM space_item_activity")?
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
