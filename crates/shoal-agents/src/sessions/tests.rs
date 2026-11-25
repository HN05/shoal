use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use futures_util::future::BoxFuture;
use shoal_core::sessions::{Host, PlaceOwnership, Session};
use shoal_protocol::space_names::SpaceSelector;

use super::*;
use crate::executions::{ExecutionKind, ExecutionRequest};
use crate::model::Workspace;
use crate::places::ExecutionPlace;
use crate::stand_in::Fixture;

/// A host whose `remaining` sessions are open, each labelled for a space,
/// which closes those it may and notes them.
#[derive(Default)]
pub(crate) struct FakeHost {
    name: Option<HostName>,
    remaining: Mutex<Vec<(String, SpaceId)>>,
    closed: Mutex<Vec<String>>,
    fail: AtomicBool,
}

impl FakeHost {
    pub(crate) fn add(&self, name: &str, space: &str) {
        let mut remaining = self.remaining.lock().unwrap();
        remaining.push((name.into(), space.into()));
    }

    pub(crate) fn closed(&self) -> Vec<String> {
        self.closed.lock().unwrap().clone()
    }

    /// Whether `owner` owns the open session `name`, as the host's labels
    /// or the daemon's record prove it.
    fn owns(&self, name: &str, owner: &Owner) -> bool {
        // A Rex block or a Herdr pane carries no labels: the daemon's
        // record proves it Shoal's.
        if matches!(self.name, Some(HostName::Rex | HostName::Herdr)) {
            return true;
        }
        let Owner::Space(space) = owner else {
            return false;
        };
        let remaining = self.remaining.lock().unwrap();
        (remaining.iter()).any(|(session, id)| session == name && id == space)
    }
}

impl Host for FakeHost {
    fn name(&self) -> HostName {
        self.name.unwrap_or(HostName::Zmx)
    }
    fn ensure_installed(&self) -> Result<()> {
        Ok(())
    }
    fn sessions<'a>(&'a self, _: &'a Path) -> BoxFuture<'a, Result<Vec<Session>>> {
        Box::pin(async {
            // Herdr labels no panes, so discovery finds none.
            if self.name == Some(HostName::Herdr) {
                return Ok(Vec::new());
            }
            let remaining = self.remaining.lock().unwrap();
            Ok((remaining.iter())
                .map(|(name, space)| Session {
                    name: name.clone(),
                    space: space.clone(),
                })
                .collect())
        })
    }

    fn close<'a>(
        &'a self,
        _: &'a Path,
        place: &'a Origin,
        owner: &'a Owner,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let name = place.id();
            let open = (self.remaining.lock().unwrap().iter()).any(|(session, _)| session == name);
            if !open || !self.owns(name, owner) {
                return Ok(());
            }
            anyhow::ensure!(
                !(self.fail.load(Ordering::Relaxed) && name == "bad"),
                "close failed"
            );
            self.closed.lock().unwrap().push(name.into());
            (self.remaining.lock().unwrap()).retain(|(session, _)| session != name);
            Ok(())
        })
    }
}

/// A workspace `removed` whose agents' places run in a fake host of each
/// of `names`.
async fn fixture_with(names: &[&'static str]) -> (Fixture, Workspace, Vec<Arc<FakeHost>>) {
    let hosts: Vec<_> = (names.iter())
        .map(|name| {
            Arc::new(FakeHost {
                name: Some(name.parse().unwrap()),
                ..Default::default()
            })
        })
        .collect();
    let given = (hosts.iter()).map(|host| host.clone() as Arc<dyn Host>);
    let fixture = Fixture::with_session_hosts(given.collect()).await;
    let workspace = fixture.workspace("removed").await;
    (fixture, workspace, hosts)
}

/// [`fixture_with`] one host, `name`.
async fn fixture_for(name: &'static str) -> (Fixture, Workspace, Arc<FakeHost>) {
    let (fixture, workspace, mut hosts) = fixture_with(&[name]).await;
    (fixture, workspace, hosts.remove(0))
}

/// [`fixture_for`] a Zmx host.
async fn fixture() -> (Fixture, Workspace, Arc<FakeHost>) {
    fixture_for("zmx").await
}

/// A Rex host whose `remaining` blocks are open, with `owned` recorded as
/// Shoal's for the workspace.
pub(crate) async fn rex_fixture(owned: &[&str]) -> (Fixture, Workspace, Arc<FakeHost>) {
    let (fixture, workspace, host) = fixture_for("rex").await;
    for block in owned {
        let place = Origin::created(HostName::Rex, (*block).into());
        let selector = SpaceSelector::id(&workspace.id);
        (fixture
            .agents
            .record_owned_place(&*fixture.daemon, &selector, place))
        .await
        .unwrap();
    }
    (fixture, workspace, host)
}

/// The request of an execution running in `origin`, an agent or not.
fn placed(origin: Origin, agent: bool) -> ExecutionRequest {
    ExecutionRequest {
        place: Some(ExecutionPlace { origin, agent }),
        ..Default::default()
    }
}

/// Start the chat `name` of the fixture's repository in `place`, which its
/// record takes as spaces record it, with its agent running there, as its
/// wrapper registers it.
async fn chat_in(fixture: &Fixture, name: &str, place: Origin) -> SpaceId {
    let repo = fixture.repository().await;
    let chat = fixture.stopped_chat(name, "chat", Some(&repo)).await;
    (fixture.agents.take_chat_place(&chat, place.clone()))
        .await
        .unwrap();
    let request = ExecutionRequest {
        agent: true,
        ..placed(place, true)
    };
    let (daemon, selector) = (&*fixture.daemon, SpaceSelector::id(&chat));
    let started = (fixture.agents)
        .begin_execution_placed(
            daemon,
            &selector,
            None,
            ExecutionKind::Command,
            None,
            request,
        )
        .await
        .unwrap();
    let agent = shoal_protocol::executions::TrackedAgent::named("claude");
    let (id, name) = (&started.plan.id, name.into());
    let running = std::time::Duration::ZERO;
    (fixture.agents)
        .track_agent(daemon, id, &agent, &chat, &name, true, running)
        .await;
    chat
}

/// The owners of the places recorded as Shoal's.
async fn owners(fixture: &Fixture) -> Vec<SpaceId> {
    let places = fixture.agents.owned_places(None).await.unwrap();
    places.into_iter().map(|(owner, _)| owner).collect()
}

#[tokio::test]
async fn removal_closes_only_the_blocks_shoal_recorded_creating() {
    let (fixture, workspace, host) = rex_fixture(&["created"]).await;
    host.add("created", workspace.id.as_str());
    // The user's own block ran an agent, but Shoal never recorded creating it.
    host.add("users", workspace.id.as_str());
    (fixture.agents)
        .close_owned_places(&*fixture.daemon, Some(workspace.id.clone()))
        .await;
    assert_eq!(host.closed(), ["created"]);
    assert!(owners(&fixture).await.is_empty());
}

#[tokio::test]
async fn the_sweep_retries_failed_closes_once_the_workspace_is_gone() {
    let (fixture, workspace, host) = rex_fixture(&["bad", "live"]).await;
    let daemon = &*fixture.daemon;
    host.add("bad", workspace.id.as_str());
    host.add("live", workspace.id.as_str());
    host.fail.store(true, Ordering::Relaxed);
    (fixture.agents)
        .close_owned_places(daemon, Some(workspace.id.clone()))
        .await;
    assert_eq!(host.closed(), ["live"]);
    assert_eq!(owners(&fixture).await.len(), 1);
    // A sweep leaves the places of a workspace that still exists.
    host.fail.store(false, Ordering::Relaxed);
    fixture.agents.cleanup_orphan_sessions(daemon).await;
    assert_eq!(host.closed(), ["live"]);
    let id = workspace.id.clone();
    (fixture.agents.store)
        .run(move |db| {
            db.execute(
                "UPDATE owned_places SET space_id='gone' WHERE space_id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    fixture.agents.cleanup_orphan_sessions(daemon).await;
    assert_eq!(host.closed(), ["live", "bad"]);
    assert!(owners(&fixture).await.is_empty());
}

#[tokio::test]
async fn places_of_hosts_without_cleanup_are_forgotten_with_their_workspace() {
    let (fixture, workspace, host) = fixture().await;
    let daemon = &*fixture.daemon;
    let place = Origin::created(HostName::Terminal, "w1:p1".into());
    let selector = SpaceSelector::id(&workspace.id);
    (fixture.agents.record_owned_place(daemon, &selector, place))
        .await
        .unwrap();
    (fixture.agents)
        .close_owned_places(daemon, Some(workspace.id.clone()))
        .await;
    assert!(host.closed().is_empty());
    assert!(owners(&fixture).await.is_empty());
}

#[tokio::test]
async fn an_agent_of_another_workspace_takes_over_a_block_shoal_created() {
    let (fixture, workspace, host) = rex_fixture(&["shared"]).await;
    let daemon = &*fixture.daemon;
    let taker = fixture.workspace("taker").await;
    host.add("shared", workspace.id.as_str());
    let shared = || Origin::created(HostName::Rex, "shared".into());
    let request = placed(shared(), true);
    let selector = SpaceSelector::id(&taker.id);
    let _started = (fixture.agents)
        .begin_execution_placed(
            daemon,
            &selector,
            None,
            ExecutionKind::Command,
            None,
            request,
        )
        .await
        .unwrap();
    // A creation record that arrives late does not take the block back.
    let selector = SpaceSelector::id(&workspace.id);
    (fixture
        .agents
        .record_owned_place(daemon, &selector, shared()))
    .await
    .unwrap();
    // The first workspace's removal and its watcher leave the block open.
    (fixture.agents)
        .close_owned_places(daemon, Some(workspace.id.clone()))
        .await;
    let close = async |space: &SpaceId| {
        let closed = fixture.agents.close_recorded_place(space, &shared()).await;
        closed.unwrap()
    };
    assert_eq!(close(&workspace.id).await, PlaceClose::OwnedElsewhere);
    assert!(host.closed().is_empty());
    // It closes with the workspace whose agent took it over.
    assert_eq!(close(&taker.id).await, PlaceClose::Closed);
    assert_eq!(host.closed(), ["shared"]);
    assert_eq!(close(&taker.id).await, PlaceClose::NotRecorded);
}

#[tokio::test]
async fn a_recorded_place_the_daemon_cannot_close_is_left_to_its_host() {
    let (fixture, workspace, host) = rex_fixture(&[]).await;
    let place = Origin::created(HostName::Terminal, "w1:p9".into());
    let selector = SpaceSelector::id(&workspace.id);
    (fixture
        .agents
        .record_owned_place(&*fixture.daemon, &selector, place.clone()))
    .await
    .unwrap();
    let closed = fixture
        .agents
        .close_recorded_place(&workspace.id, &place)
        .await;
    assert_eq!(closed.unwrap(), PlaceClose::HostCloses);
    assert!(host.closed().is_empty());
    assert!(owners(&fixture).await.is_empty());
}

#[tokio::test]
async fn a_nested_command_or_a_chat_leaves_the_owner_of_a_block() {
    let (fixture, workspace, host) = rex_fixture(&["parent", "chatted"]).await;
    let daemon = &*fixture.daemon;
    let child = fixture.workspace("child").await;
    host.add("parent", workspace.id.as_str());
    host.add("chatted", workspace.id.as_str());
    let block = |name: &str| Origin::created(HostName::Rex, name.into());
    // A parent agent's `shoal run --workspace child` runs in the parent's block.
    let request = placed(block("parent"), false);
    let selector = SpaceSelector::id(&child.id);
    let _nested = (fixture.agents)
        .begin_execution_placed(
            daemon,
            &selector,
            None,
            ExecutionKind::Command,
            None,
            request,
        )
        .await
        .unwrap();
    let close = async |space: &SpaceId, name: &str| {
        let closed = fixture
            .agents
            .close_recorded_place(space, &block(name))
            .await;
        closed.unwrap()
    };
    assert_eq!(close(&child.id, "parent").await, PlaceClose::OwnedElsewhere);
    // A chat that starts in a block takes it from the workspace.
    let mut chat_place = block("chatted");
    chat_place.owned = false;
    chat_in(&fixture, "chat", chat_place).await;
    (fixture.agents)
        .close_owned_places(daemon, Some(workspace.id.clone()))
        .await;
    assert_eq!(host.closed(), ["parent"]);
    fixture.agents.cleanup_orphan_sessions(daemon).await;
    let closed = close(&workspace.id, "chatted").await;
    assert_eq!(closed, PlaceClose::OwnedElsewhere);
    assert_eq!(host.closed(), ["parent"]);
}

#[tokio::test]
async fn a_session_closes_with_the_workspace_its_host_labels_it_for() {
    let (fixture, workspace, host) = fixture().await;
    let other = fixture.workspace("other").await;
    host.add("labelled", workspace.id.as_str());
    let ownership = async |space: &SpaceId, host, name: &str| {
        let session = Origin::created(host, name.into());
        let selector = SpaceSelector::id(space);
        (fixture
            .agents
            .place_ownership(&*fixture.daemon, &selector, &session))
        .await
        .unwrap()
    };
    let space = PlaceOwnership::Space;
    assert!(ownership(&workspace.id, HostName::Zmx, "labelled").await == space);
    assert!(ownership(&other.id, HostName::Zmx, "labelled").await != space);
    assert!(ownership(&workspace.id, HostName::Zmx, "unlabelled").await != space);
    assert!(ownership(&workspace.id, HostName::Rex, "labelled").await != space);
}

#[tokio::test]
async fn panes_of_different_servers_under_one_id_are_different_places() {
    let (fixture, workspace, host) = fixture_for("herdr").await;
    let daemon = &*fixture.daemon;
    let taker = fixture.workspace("taker").await;
    let on = |server: &str| {
        let mut pane = Origin::created(HostName::Herdr, "w1:p1".into());
        pane.data.insert("HERDR_SOCKET_PATH".into(), server.into());
        pane
    };
    let selector = SpaceSelector::id(&workspace.id);
    (fixture
        .agents
        .record_owned_place(daemon, &selector, on("first")))
    .await
    .unwrap();
    host.add("w1:p1", workspace.id.as_str());
    // An agent in the user's pane of the same ID on another server takes
    // nothing over, and records nothing.
    let mut users = on("second");
    users.owned = false;
    let request = placed(users.clone(), true);
    let taking = SpaceSelector::id(&taker.id);
    let _started = (fixture.agents)
        .begin_execution_placed(daemon, &taking, None, ExecutionKind::Command, None, request)
        .await
        .unwrap();
    let ownership = async |space: &SpaceId, place: &Origin| {
        let selector = SpaceSelector::id(space);
        (fixture.agents.place_ownership(daemon, &selector, place))
            .await
            .unwrap()
    };
    let space = PlaceOwnership::Space;
    assert!(ownership(&workspace.id, &on("first")).await == space);
    // A variable of the pane's that names no server leaves it the same place.
    let mut shell = on("first");
    shell.data.insert("HERDR_THEME".into(), "dark".into());
    assert!(ownership(&workspace.id, &shell).await == space);
    assert!(ownership(&taker.id, &on("first")).await != space);
    assert!(ownership(&workspace.id, &users).await != space);
    (fixture.agents)
        .close_owned_places(daemon, Some(taker.id.clone()))
        .await;
    assert!(host.closed().is_empty());
    (fixture.agents)
        .close_owned_places(daemon, Some(workspace.id.clone()))
        .await;
    assert_eq!(host.closed(), ["w1:p1"]);
}

#[tokio::test]
async fn a_session_its_host_labels_closes_through_the_host_without_a_record() {
    let (fixture, workspace, host) = fixture().await;
    let other = fixture.workspace("other").await;
    host.add("labelled", workspace.id.as_str());
    let session = Origin::created(HostName::Zmx, "labelled".into());
    let close = async |space: &SpaceId| {
        let closed = fixture.agents.close_recorded_place(space, &session).await;
        closed.unwrap()
    };
    assert_eq!(close(&other.id).await, PlaceClose::NotRecorded);
    assert!(host.closed().is_empty());
    assert_eq!(close(&workspace.id).await, PlaceClose::Closed);
    assert_eq!(host.closed(), ["labelled"]);
}

#[tokio::test]
async fn the_sweep_leaves_a_chats_places_while_it_is_recorded() {
    let (fixture, workspace, host) = fixture_for("rex").await;
    host.add("opened", workspace.id.as_str());
    let place = Origin::created(HostName::Rex, "opened".into());
    let chat = chat_in(&fixture, "kept", place).await;
    fixture
        .agents
        .cleanup_orphan_sessions(&*fixture.daemon)
        .await;
    assert!(host.closed().is_empty());
    assert_eq!(owners(&fixture).await, [chat]);
}

#[tokio::test]
async fn a_session_a_chat_recorded_taking_over_stays_open_until_it_relabels_it() {
    let (fixture, workspace, host) = fixture().await;
    let daemon = &*fixture.daemon;
    // Each chat recorded its session before relabelling it: one still shows
    // the workspace's label, the other a removed space's.
    host.add("taken", workspace.id.as_str());
    host.add("orphaned", "gone");
    for (name, session) in [("taker", "taken"), ("adopter", "orphaned")] {
        let place = Origin::created(HostName::Zmx, session.into());
        chat_in(&fixture, name, place).await;
    }
    (fixture.agents)
        .close_space_places(daemon, &workspace.id, &workspace.name)
        .await;
    fixture.agents.cleanup_orphan_sessions(daemon).await;
    assert!(host.closed().is_empty());
    assert_eq!(owners(&fixture).await.len(), 2);
}

#[tokio::test]
async fn a_labelled_session_a_chat_runs_in_stays_open_before_it_is_recorded() {
    let (fixture, workspace, host) = fixture().await;
    let daemon = &*fixture.daemon;
    // Each chat started in place in a session no record names, which still
    // shows the workspace's label or a removed space's.
    host.add("labelled", workspace.id.as_str());
    host.add("orphaned", "gone");
    for (name, session) in [("guest", "labelled"), ("stray", "orphaned")] {
        let place = Origin {
            owned: false,
            ..Origin::created(HostName::Zmx, session.into())
        };
        chat_in(&fixture, name, place).await;
    }
    (fixture.agents)
        .close_space_places(daemon, &workspace.id, &workspace.name)
        .await;
    fixture.agents.cleanup_orphan_sessions(daemon).await;
    assert!(host.closed().is_empty());
}
