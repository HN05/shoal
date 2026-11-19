//! The records a subscription covers, as they are now: those of the topics
//! it asked for that its caller may read.
use shoal_ids::space_ids::SpaceId;
use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;

use super::agents;
use crate::{
    daemon::{
        agent_messages::AgentMessage,
        approvals::DecisionStatus,
        manager::Manager,
        notifications::{Alert, AlertKind},
        scope::Caller,
    },
    model::{Relation, Space},
};
use shoal_forge::updates::Snapshot;
use shoal_protocol::{
    log::LogTopic,
    subscriptions::records::{
        Check, Link, LookupError, Message, MessageRecipient, Record, RecordKey, ReportedSessions,
        SpaceRecord, Topic,
    },
};

/// Records by key, each with its JSON, which tells a changed one apart.
pub(super) type Records = BTreeMap<RecordKey, (Record, serde_json::Value)>;

/// The topics a subscription covers: those it asked for, without
/// notifications for a scoped caller, and the log of a space it may read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Topics {
    topics: HashSet<Topic>,
    log: Option<LogTopic>,
}

impl Topics {
    pub(super) fn new(topics: &[Topic], log: Option<LogTopic>, caller: Option<&Caller>) -> Self {
        let topics = topics
            .iter()
            .copied()
            .filter(|topic| caller.is_none() || *topic != Topic::Notifications)
            .collect();
        Self { topics, log }
    }

    fn has(&self, topic: Topic) -> bool {
        self.topics.contains(&topic)
    }
}

/// The records of `topics` that `caller` may read.
pub(super) async fn take(
    manager: &Manager,
    caller: Option<&Caller>,
    topics: &Topics,
) -> Result<Records> {
    let visible = Visible::load(manager, caller, topics).await?;
    let mut found = Vec::new();
    if topics.has(Topic::Repositories) {
        found.extend(repositories(manager, caller).await?);
    }
    if topics.has(Topic::Spaces) {
        found.extend(space_records(manager, &visible.spaces).await?);
    }
    if topics.has(Topic::Links) {
        found.extend(links(manager, &visible.spaces).await?);
    }
    if topics.has(Topic::Sessions) {
        found.extend(sessions(manager, &visible.spaces).await?);
    }
    if topics.has(Topic::Questions) {
        found.extend(questions(manager, &visible).await?);
    }
    if topics.has(Topic::Approvals) {
        found.extend(approvals(manager, caller, &visible).await?);
    }
    if topics.has(Topic::Messages) {
        found.extend(messages(manager, caller, &visible).await?);
    }
    if topics.has(Topic::Notifications) {
        found.extend(notifications(manager).await?);
    }
    if let Some(log) = &topics.log {
        let entries = manager
            .log(log.space.as_ref(), log.filter, log.limit)
            .await?;
        found.extend(
            entries
                .into_iter()
                .map(|entry| Record::LogEntry(Box::new(entry))),
        );
    }
    let mut records = Records::new();
    for record in found {
        let value = serde_json::to_value(&record)?;
        records.insert(record.key(), (record, value));
    }
    Ok(records)
}

/// The spaces a caller may read: every space, or those a scoped caller
/// sees, read only for the topics that need them.
#[derive(Default)]
struct Visible {
    spaces: Vec<Space>,
}

impl Visible {
    async fn load(manager: &Manager, caller: Option<&Caller>, topics: &Topics) -> Result<Self> {
        let reading_spaces = [
            Topic::Spaces,
            Topic::Links,
            Topic::Sessions,
            Topic::Questions,
            Topic::Approvals,
            Topic::Messages,
        ];
        if !reading_spaces.into_iter().any(|topic| topics.has(topic)) {
            return Ok(Self::default());
        }
        let spaces = manager.visible_spaces(caller).await?;
        Ok(Self { spaces })
    }

    fn contains(&self, id: &SpaceId) -> bool {
        self.space(id).is_some()
    }

    /// The space `id` among them.
    fn space(&self, id: &SpaceId) -> Option<&Space> {
        self.spaces.iter().find(|space| space.id == *id)
    }
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

async fn space_records(manager: &Manager, spaces: &[Space]) -> Result<Vec<Record>> {
    let agents = agents::load(manager, spaces).await?;
    Ok(spaces
        .iter()
        .map(|space| {
            Record::Space(Box::new(SpaceRecord {
                agent: agents.activity(space),
                space: space.clone(),
            }))
        })
        .collect())
}

async fn links(manager: &Manager, spaces: &[Space]) -> Result<Vec<Record>> {
    let mut activity = item_activity(manager).await?;
    Ok(spaces
        .iter()
        .flat_map(|space| {
            let observed = activity.remove(&space.id).unwrap_or_default();
            space_links(space, &observed)
        })
        .map(|link| Record::Link(Box::new(link)))
        .collect())
}

/// Each item `space` links, with what the daemon last observed of it on its
/// forge, from `activity` by URL.
fn space_links(space: &Space, activity: &HashMap<String, Snapshot>) -> Vec<Link> {
    let links = &space.links;
    let assigned = (links.issues.iter().chain(&links.prs)).map(|item| {
        (
            Relation::Assigned,
            item.kind(),
            &item.url,
            item.title.clone(),
        )
    });
    let recorded = [
        (Relation::Opened, &links.opened),
        (Relation::Related, &links.related),
    ]
    .into_iter()
    .flat_map(|(relation, items)| {
        items
            .iter()
            .map(move |item| (relation, item.kind(), &item.url, item.title.clone()))
    });
    assigned
        .chain(recorded)
        .map(|(relation, kind, url, title)| {
            let observed = activity.get(url.url());
            Link {
                id: format!("{}/{relation}/{url}", space.id),
                space_id: space.id.clone(),
                kind,
                url: url.clone(),
                relation,
                title,
                state: observed.and_then(Snapshot::state),
                head: observed.and_then(Snapshot::head).cloned(),
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
        .map(|(name, state)| Check {
            name: name.into(),
            state,
            complete: state.complete(),
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

/// The sessions the agents of each of `spaces` reported.
async fn sessions(manager: &Manager, spaces: &[Space]) -> Result<Vec<Record>> {
    let mut records = Vec::new();
    for space in spaces {
        records.push(Record::Sessions(ReportedSessions {
            space_id: space.id.clone(),
            sessions: manager.reported_sessions(&space.id).await?,
        }));
    }
    Ok(records)
}

/// The open questions of the visible spaces.
async fn questions(manager: &Manager, visible: &Visible) -> Result<Vec<Record>> {
    Ok(manager
        .open_questions()
        .await?
        .into_iter()
        .filter(|question| visible.contains(&question.space_id))
        .map(Record::Question)
        .collect())
}

/// Pending approval requests: all of them for the user, including issue
/// proposals that outlive their space, and for a scoped caller those its
/// visible spaces made.
async fn approvals(
    manager: &Manager,
    caller: Option<&Caller>,
    visible: &Visible,
) -> Result<Vec<Record>> {
    Ok(manager
        .approval_requests(None)
        .await?
        .into_iter()
        .filter(|request| {
            request.status == DecisionStatus::Pending
                && (caller.is_none() || visible.contains(&request.space_id))
        })
        .map(|request| Record::Approval(Box::new(request)))
        .collect())
}

/// Messages queued for the agents of the visible spaces, and for the user,
/// who alone reads the messages agents send it.
async fn messages(
    manager: &Manager,
    caller: Option<&Caller>,
    visible: &Visible,
) -> Result<Vec<Record>> {
    let mut messages: Vec<Record> = (manager.store)
        .read(|db| crate::daemon::agent_messages::every_unread(db))
        .await?
        .into_iter()
        .filter_map(|(space_id, message)| {
            let space = visible.space(&space_id)?;
            Some(Record::Message(agent_message(message, space)))
        })
        .collect();
    if caller.is_none() {
        messages.extend(
            manager
                .notifications
                .list(false, u32::MAX)
                .await?
                .iter()
                .filter(|notification| notification.kind == AlertKind::AgentMessage)
                .map(|notification| Record::Message(user_message(notification))),
        );
    }
    Ok(messages)
}

/// A message queued for the agent of `space`.
fn agent_message(message: AgentMessage, space: &Space) -> Message {
    Message {
        id: format!("agent/{}", message.id),
        recipient: MessageRecipient::Agent,
        space_id: Some(space.id.clone()),
        space: Some(space.name.clone()),
        text: message.message,
        attachments: message.attachments,
        created_at: message.created_at,
    }
}

/// A space's message to the user, which its notification records.
fn user_message(notification: &Alert) -> Message {
    Message {
        id: format!("user/{}", notification.id),
        recipient: MessageRecipient::User,
        space_id: None,
        space: notification.workspace.clone(),
        text: notification.message.clone(),
        attachments: Vec::new(),
        created_at: notification.created_at,
    }
}

async fn notifications(manager: &Manager) -> Result<Vec<Record>> {
    Ok(manager
        .notifications
        .list(false, u32::MAX)
        .await?
        .into_iter()
        .map(Record::Alert)
        .collect())
}

/// What the daemon last observed of each linked item on its forge, by space
/// ID and URL.
async fn item_activity(manager: &Manager) -> Result<HashMap<SpaceId, HashMap<String, Snapshot>>> {
    let rows = manager
        .store
        .run(|db| {
            Ok(db
                .prepare("SELECT space_id,url,record FROM space_item_activity")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<rusqlite::Result<Vec<(SpaceId, String, String)>>>()?)
        })
        .await?;
    let mut activity: HashMap<SpaceId, HashMap<String, Snapshot>> = HashMap::new();
    for (space, url, record) in rows {
        // An unreadable observation shows the link without one.
        if let Ok(snapshot) = serde_json::from_str(&record) {
            activity.entry(space).or_default().insert(url, snapshot);
        }
    }
    Ok(activity)
}
