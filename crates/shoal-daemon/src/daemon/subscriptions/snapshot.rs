//! The records a subscription covers, as they are now: those of the topics
//! it asked for that its caller may read.
use shoal_ids::space_ids::SpaceId;
use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;

use super::agents;
use crate::{
    daemon::{approvals::DecisionStatus, manager::Manager, scope::Caller},
    model::{Relation, Space},
};
use shoal_forge::updates::Snapshot;
use shoal_protocol::{
    log::LogTopic,
    subscriptions::records::{
        Check, Link, LookupError, Record, RecordKey, ReportedSessions, SpaceRecord, Topic,
    },
};

/// Records by key, each with its JSON, which tells a changed one apart.
pub(super) type Records = BTreeMap<RecordKey, (Record, serde_json::Value)>;

/// The topics a subscription covers: those it asked for, and the log of a
/// space it may read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Topics {
    topics: HashSet<Topic>,
    log: Option<LogTopic>,
}

impl Topics {
    pub(super) fn new(topics: &[Topic], log: Option<LogTopic>) -> Self {
        let topics = topics.iter().copied().collect();
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
    if topics.has(Topic::Threads) {
        found.extend(threads(manager, caller, &visible).await?);
    }
    if topics.has(Topic::Approvals) {
        found.extend(approvals(manager, caller, &visible).await?);
    }
    if topics.has(Topic::Alerts) {
        found.extend(alerts(manager, caller, &visible).await?);
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
            Topic::Threads,
            Topic::Approvals,
            Topic::Alerts,
        ];
        if !reading_spaces.into_iter().any(|topic| topics.has(topic)) {
            return Ok(Self::default());
        }
        let spaces = manager.coordination.visible_spaces(manager, caller).await?;
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
        .coordination
        .visible_repositories(manager, caller)
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
            sessions: manager.spaces.reported_sessions(&space.id).await?,
        }));
    }
    Ok(records)
}

/// The open threads with a post of the visible spaces, but a pending
/// review's comments that `caller` does not read.
async fn threads(
    manager: &Manager,
    caller: Option<&Caller>,
    visible: &Visible,
) -> Result<Vec<Record>> {
    let mut found = Vec::new();
    for thread in manager.coordination.open_threads().await? {
        if visible.contains(&thread.space_id)
            && manager
                .coordination
                .reads_held(manager, caller, &thread)
                .await?
        {
            found.push(Record::Thread(Box::new(thread)));
        }
    }
    Ok(found)
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
        .coordination
        .approval_requests(manager, None)
        .await?
        .into_iter()
        .filter(|request| {
            request.status == DecisionStatus::Pending
                && (caller.is_none() || visible.contains(&request.space_id))
        })
        .map(|request| Record::Approval(Box::new(request)))
        .collect())
}

/// Unread alerts to the agents of the visible spaces, and for the user
/// every alert to the user.
async fn alerts(
    manager: &Manager,
    caller: Option<&Caller>,
    visible: &Visible,
) -> Result<Vec<Record>> {
    let mut alerts: Vec<Record> = (manager.store)
        .read(|db| crate::daemon::messages::every_unread(db))
        .await?
        .into_iter()
        .filter(|alert| {
            (alert.recipient.as_ref()).is_some_and(|recipient| visible.contains(&recipient.id))
        })
        .map(Record::Alert)
        .collect();
    if caller.is_none() {
        let users = manager.notifications.list(false, u32::MAX).await?;
        alerts.extend(users.into_iter().map(Record::Alert));
    }
    Ok(alerts)
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
