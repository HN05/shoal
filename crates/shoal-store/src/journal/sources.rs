//! What each trigger passes of the row it saw, keyed by that row's own
//! column names, and the event Rust builds from it.
use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::events::{
    AgentStateEvent, DemoEvent, EventKind, HandoffEvent, ReviewEvent, SimulatorEvent, SpaceEvent,
    ThreadEvent,
};
use shoal_protocol::item_numbers::ItemNumber;
use shoal_protocol::model::{EventCause, SpaceRef};
use shoal_protocol::transcripts::HandedOverSession;

/// An event a trigger records, with what caused it and its error.
pub(super) struct Triggered {
    pub(super) event: SpaceEvent,
    pub(super) cause: Option<EventCause>,
    pub(super) error: Option<String>,
}

impl From<SpaceEvent> for Triggered {
    fn from(event: SpaceEvent) -> Self {
        Self {
            event,
            cause: None,
            error: None,
        }
    }
}

/// The event of `kind` that the trigger of the space `id` records from
/// `source`; none when what it names is gone.
pub(super) fn event(
    db: &Connection,
    kind: EventKind,
    id: &SpaceId,
    source: &str,
) -> Result<Option<Triggered>> {
    let build = match kind {
        EventKind::Created => built::<Created>,
        EventKind::Ready | EventKind::Undone | EventKind::Renamed | EventKind::BranchChanged => {
            built::<Empty>
        }
        EventKind::SetupFailed => built::<Failed>,
        EventKind::Completed => built::<Completion>,
        EventKind::BaseChanged => built::<RemovedBase>,
        EventKind::ParentChanged => built::<Parent>,
        EventKind::ChildCreated | EventKind::ChildRemoved => built::<Child>,
        EventKind::ReviewReady | EventKind::ReviewCleared => built::<Review>,
        EventKind::AgentState => built::<State>,
        EventKind::DemoStarted | EventKind::DemoStopped => built::<Demo>,
        EventKind::SimulatorAcquired | EventKind::SimulatorReleased => built::<Simulator>,
        EventKind::ThreadOpened | EventKind::ThreadResolved => built::<Thread>,
        EventKind::ThreadPosted | EventKind::ThreadForwarded => built::<Post>,
        EventKind::Converted => built::<Conversion>,
        other => bail!("no trigger records {other}"),
    };
    build(db, kind, id, source)
}

/// A trigger's source row, which builds the event of `kind` it records for
/// the space `id`; none when what it names is gone.
trait Source: DeserializeOwned {
    fn event(self, db: &Connection, kind: EventKind, id: &SpaceId) -> Result<Option<Triggered>>;
}

/// The event `source`, read as the row type `T`, records.
fn built<T: Source>(
    db: &Connection,
    kind: EventKind,
    id: &SpaceId,
    source: &str,
) -> Result<Option<Triggered>> {
    let row: T =
        serde_json::from_str(source).with_context(|| format!("invalid trigger source {source}"))?;
    row.event(db, kind, id)
}

/// The refusal of `kind` from a source type that records other kinds.
fn unexpected(kind: EventKind) -> Result<Option<Triggered>> {
    bail!("no trigger source of this shape records {kind}")
}

fn recorded(event: SpaceEvent) -> Result<Option<Triggered>> {
    Ok(Some(event.into()))
}

/// The space `id` as an event names another space.
fn space_ref(db: &Connection, id: &SpaceId) -> Result<Option<SpaceRef>> {
    Ok(db
        .query_row(
            "SELECT spaces.id,spaces.name,spaces.kind,workspaces.branch FROM spaces
                LEFT JOIN workspaces ON workspaces.id=spaces.id AND spaces.kind='workspace'
                WHERE spaces.id=?1",
            [id],
            crate::tree::space_ref,
        )
        .optional()?)
}

/// A space's creation, which reads its base and parent as they are
/// recorded now.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Created {}

impl Source for Created {
    fn event(self, db: &Connection, _: EventKind, id: &SpaceId) -> Result<Option<Triggered>> {
        let base_ref: Option<String> = db
            .query_row("SELECT base_ref FROM workspaces WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()?
            .flatten();
        let parent: Option<SpaceId> = db
            .query_row(
                "SELECT parent_id FROM space_parents WHERE space_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        let parent = match parent {
            Some(parent) => space_ref(db, &parent)?,
            None => None,
        };
        recorded(SpaceEvent::Created {
            base_ref,
            base_workspace: super::base_of(db, id)?,
            parent,
        })
    }
}

/// A source with no columns, for a kind that records nothing of its own.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

impl Source for Empty {
    fn event(self, _: &Connection, kind: EventKind, _: &SpaceId) -> Result<Option<Triggered>> {
        recorded(match kind {
            EventKind::Ready => SpaceEvent::Ready,
            EventKind::Undone => SpaceEvent::Undone,
            EventKind::Renamed => SpaceEvent::Renamed,
            EventKind::BranchChanged => SpaceEvent::BranchChanged,
            other => return unexpected(other),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Failed {
    error: Option<String>,
}

impl Source for Failed {
    fn event(self, _: &Connection, _: EventKind, _: &SpaceId) -> Result<Option<Triggered>> {
        Ok(Some(Triggered {
            error: self.error,
            ..SpaceEvent::SetupFailed.into()
        }))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    cause: EventCause,
}

impl Source for Completion {
    fn event(self, _: &Connection, _: EventKind, _: &SpaceId) -> Result<Option<Triggered>> {
        Ok(Some(Triggered {
            cause: Some(self.cause),
            ..SpaceEvent::Completed.into()
        }))
    }
}

/// The removed workspace's own base, which the workspaces on it take.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemovedBase {
    base_workspace_id: Option<SpaceId>,
}

impl Source for RemovedBase {
    fn event(self, db: &Connection, _: EventKind, _: &SpaceId) -> Result<Option<Triggered>> {
        let base_workspace = match self.base_workspace_id {
            Some(base) => super::workspace_ref(db, &base)?,
            None => None,
        };
        Ok(Some(Triggered {
            cause: Some(EventCause::Removed),
            ..SpaceEvent::BaseChanged { base_workspace }.into()
        }))
    }
}

/// The parent the space has now; none once it has none.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Parent {
    parent_id: Option<SpaceId>,
}

impl Source for Parent {
    fn event(self, db: &Connection, _: EventKind, _: &SpaceId) -> Result<Option<Triggered>> {
        let parent = match self.parent_id {
            Some(parent) => space_ref(db, &parent)?,
            None => None,
        };
        recorded(SpaceEvent::ParentChanged { parent })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Child {
    id: SpaceId,
}

impl Source for Child {
    fn event(self, db: &Connection, kind: EventKind, _: &SpaceId) -> Result<Option<Triggered>> {
        let Some(child) = space_ref(db, &self.id)? else {
            return Ok(None);
        };
        recorded(match kind {
            EventKind::ChildCreated => SpaceEvent::ChildCreated { child },
            EventKind::ChildRemoved => SpaceEvent::ChildRemoved { child },
            other => return unexpected(other),
        })
    }
}

/// A ready mark as its row holds it, by the same names its event gives.
#[derive(Deserialize)]
#[serde(transparent)]
struct Review(ReviewEvent);

impl Source for Review {
    fn event(self, _: &Connection, kind: EventKind, _: &SpaceId) -> Result<Option<Triggered>> {
        let mut review = self.0;
        // An empty URL marks the space itself.
        review.url = review.url.filter(|url| !url.is_empty());
        recorded(match kind {
            EventKind::ReviewReady => SpaceEvent::ReviewReady { review },
            EventKind::ReviewCleared => SpaceEvent::ReviewCleared { review },
            other => return unexpected(other),
        })
    }
}

/// The agent's turn state now, by the name its event gives it; none once
/// its execution ended.
#[derive(Deserialize)]
#[serde(transparent)]
struct State(AgentStateEvent);

impl Source for State {
    fn event(self, _: &Connection, _: EventKind, _: &SpaceId) -> Result<Option<Triggered>> {
        recorded(SpaceEvent::AgentState {
            agent_state: self.0,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Demo {
    name: String,
    record: DemoRecord,
}

/// What a demo's record says of its preset and the simulator it runs on.
#[derive(Deserialize)]
struct DemoRecord {
    #[serde(default)]
    preset: Option<shoal_protocol::demos::Preset>,
    #[serde(default)]
    environment: BTreeMap<String, String>,
}

impl Source for Demo {
    fn event(mut self, _: &Connection, kind: EventKind, _: &SpaceId) -> Result<Option<Triggered>> {
        let demo = DemoEvent {
            udid: self.record.environment.remove("SHOAL_SIM_UDID"),
            name: self.name,
            preset: self.record.preset,
        };
        recorded(match kind {
            EventKind::DemoStarted => SpaceEvent::DemoStarted { demo },
            EventKind::DemoStopped => SpaceEvent::DemoStopped { demo },
            other => return unexpected(other),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Simulator {
    record: SimulatorRecord,
}

/// What a simulator's record says of its lease and device.
#[derive(Deserialize)]
struct SimulatorRecord {
    lease_name: Option<String>,
    udid: Option<String>,
    device: String,
    runtime: String,
}

impl Source for Simulator {
    fn event(self, _: &Connection, kind: EventKind, _: &SpaceId) -> Result<Option<Triggered>> {
        let simulator = Box::new(SimulatorEvent {
            name: self.record.lease_name.unwrap_or_default(),
            udid: self.record.udid,
            device: self.record.device,
            runtime: self.record.runtime,
        });
        recorded(match kind {
            EventKind::SimulatorAcquired => SpaceEvent::SimulatorAcquired { simulator },
            EventKind::SimulatorReleased => SpaceEvent::SimulatorReleased { simulator },
            other => return unexpected(other),
        })
    }
}

/// A thread as its row holds it, with its outcome once resolved.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Thread {
    space_number: Option<SpaceNumber>,
    number: u32,
    name: String,
    #[serde(default)]
    outcome: Option<Outcome>,
}

#[derive(Deserialize)]
struct Outcome {
    outcome: shoal_protocol::threads::OutcomeKind,
}

impl Source for Thread {
    fn event(self, _: &Connection, kind: EventKind, _: &SpaceId) -> Result<Option<Triggered>> {
        let Some(thread) = self.thread(kind).map(Box::new) else {
            return Ok(None);
        };
        recorded(match kind {
            EventKind::ThreadOpened => SpaceEvent::ThreadOpened { thread },
            EventKind::ThreadResolved => SpaceEvent::ThreadResolved { thread },
            other => return unexpected(other),
        })
    }
}

impl Thread {
    /// The thread's part of its event, as `kind` names what happened to
    /// it; none for a thread its space's number no longer numbers.
    fn thread(self, kind: EventKind) -> Option<ThreadEvent> {
        let outcome = match kind {
            EventKind::ThreadResolved => self.outcome.map(|outcome| outcome.outcome),
            _ => None,
        };
        Some(ThreadEvent {
            number: ItemNumber {
                space: self.space_number?,
                item: self.number,
            },
            name: self.name,
            text: None,
            sender: None,
            to: None,
            outcome,
        })
    }
}

/// A post as its row holds it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Post {
    thread_id: i64,
    text: String,
    sender: String,
    sender_name: Option<String>,
    forwarded_to: Option<SpaceId>,
    forwarded_name: Option<String>,
}

impl Source for Post {
    fn event(self, db: &Connection, kind: EventKind, _: &SpaceId) -> Result<Option<Triggered>> {
        let forwarded = kind == EventKind::ThreadForwarded;
        let Some(thread) = self.thread(db, forwarded)?.map(Box::new) else {
            return Ok(None);
        };
        recorded(match kind {
            EventKind::ThreadPosted => SpaceEvent::ThreadPosted { thread },
            EventKind::ThreadForwarded => SpaceEvent::ThreadForwarded { thread },
            other => return unexpected(other),
        })
    }
}

impl Post {
    /// The post's part of its event, in its thread, naming whom it went to
    /// when it `forwarded` the thread; none once the thread is gone or no
    /// longer numbered.
    fn thread(self, db: &Connection, forwarded: bool) -> Result<Option<ThreadEvent>> {
        let thread = db
            .query_row(
                "SELECT space_number,number,name FROM threads WHERE id=?1",
                [self.thread_id],
                |row| {
                    Ok(Thread {
                        space_number: row.get(0)?,
                        number: row.get(1)?,
                        name: row.get(2)?,
                        outcome: None,
                    })
                },
            )
            .optional()?;
        let Some(mut event) = thread.and_then(|thread| thread.thread(EventKind::ThreadPosted))
        else {
            return Ok(None);
        };
        event.to = match forwarded {
            true => Some(self.recipient(db)?),
            false => None,
        };
        event.sender = Some(self.sender_name.unwrap_or_else(|| {
            match self.sender.as_str() {
                "user" => "the user",
                _ => "Shoal",
            }
            .to_owned()
        }));
        event.text = Some(self.text);
        Ok(Some(event))
    }

    /// Whom a forward went to, as text names them.
    fn recipient(&self, db: &Connection) -> Result<String> {
        let Some(space) = &self.forwarded_to else {
            return Ok("the user".to_owned());
        };
        let name: Option<String> = db
            .query_row("SELECT name FROM spaces WHERE id=?1", [space], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(name
            .or_else(|| self.forwarded_name.clone())
            .unwrap_or_else(|| space.to_string()))
    }
}

/// A conversion of the space's kind, without columns, or a handoff to
/// another agent tool as its row holds it once the new session started.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Conversion {
    #[serde(default)]
    record: Option<HandoffRecord>,
    #[serde(default)]
    session: Option<shoal_ids::agent_session_ids::AgentSessionId>,
}

#[derive(Deserialize)]
struct HandoffRecord {
    from: HandedOverSession,
    to: HandedOverSession,
}

impl Source for Conversion {
    fn event(self, _: &Connection, _: EventKind, _: &SpaceId) -> Result<Option<Triggered>> {
        let handoff = self.handoff().map(Box::new);
        recorded(SpaceEvent::Converted { handoff })
    }
}

impl Conversion {
    fn handoff(self) -> Option<HandoffEvent> {
        let record = self.record?;
        let mut to = record.to;
        to.session = self.session;
        Some(HandoffEvent {
            from: record.from,
            to,
        })
    }
}
