//! The agent sessions hosts keep: finding them where the daemon recorded a
//! workspace's or chat's agents, and attaching this terminal to one.
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::{
    config::Effective,
    model::{Execution, Protection, RunningAgent},
    sessions::{HostName, Origin, OriginExt as _},
    terminal::{Elsewhere, Host, Places, Session, Sessions, enabled_host, named},
};

/// A place the daemon recorded, with the agent it recorded running there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recorded<'a> {
    place: &'a Origin,
    agent: Option<&'a crate::agent::Agent>,
    /// The execution recorded there.
    execution: Option<&'a ExecutionId>,
    /// Why Shoal stopped that agent, while its wrapper waits to restore it.
    stopped: Option<&'a Protection>,
}

impl<'a> Recorded<'a> {
    /// Where a space's `executions` were recorded running, with their
    /// agents, and the `places` Shoal created for it, a workspace's or a
    /// chat's.
    pub fn space(executions: &'a [Execution], places: &'a [Origin]) -> Vec<Self> {
        let executions = executions.iter().filter_map(|execution| {
            Some(Self {
                place: execution.origin.as_ref()?,
                agent: execution.agent.as_ref(),
                execution: Some(&execution.id),
                stopped: execution.stopped.as_ref(),
            })
        });
        let created = places.iter().map(|place| Self {
            place,
            agent: None,
            execution: None,
            stopped: None,
        });
        executions.chain(created).collect()
    }
}

/// The agent sessions still open at `places` in the hosts `settings`
/// enable, where the daemon recorded a space's executions running, or the
/// places Shoal created for it, and in the sessions nested in them. A
/// session where a stopped agent waits to be restored says so, whichever
/// place found it.
pub async fn sessions(settings: &Effective, places: &[Recorded<'_>]) -> Result<Vec<Session>> {
    let places = distinct_places(places);
    let origins: Vec<_> = places.iter().map(|recorded| recorded.place).collect();
    let (found, failure) = open_with_nested(&origins, |host| {
        enabled_host(settings, host).and_then(Host::sessions)
    })
    .await;
    if let Some(error) = failure {
        return Err(error);
    }
    let mut sessions: Vec<_> = places
        .iter()
        .zip(found)
        .filter_map(|(recorded, session)| {
            Some(Session {
                agent: recorded.agent.cloned(),
                execution: recorded.execution.cloned(),
                stopped: recorded.stopped.cloned(),
                ..session?
            })
        })
        .collect();
    mark_stopped(&mut sessions);
    Ok(sessions)
}

/// The session open at each of `places`, in order, as [`open_at`] finds
/// them, each shown with the session nested in it as [`with_nested`] does.
async fn open_with_nested(
    places: &[&Origin],
    sessions_of: impl Fn(HostName) -> Option<&'static dyn Sessions>,
) -> (Vec<Option<Session>>, Option<anyhow::Error>) {
    let nested = places.iter().filter_map(|place| place.nested.as_deref());
    let asked: Vec<_> = places.iter().copied().chain(nested).collect();
    let (found, failure) = open_at(&asked, sessions_of).await;
    let mut found = found.into_iter();
    let outer: Vec<_> = found.by_ref().take(places.len()).collect();
    let shown = places
        .iter()
        .zip(outer)
        .map(|(place, session)| {
            let nested = place.nested.as_ref().and_then(|_| found.next().flatten());
            with_nested(session, nested)
        })
        .collect();
    (shown, failure)
}

/// The session a recorded place shows, from `outer`, the one open at the
/// place, and `nested`, the one open in the session nested in it: the
/// place's while it is open and a client of the session is attached, which
/// the place's own is, attached while a terminal shows either, and naming
/// the nested session only while that is open; else the nested session,
/// which outlives the place and a detach there.
fn with_nested(outer: Option<Session>, nested: Option<Session>) -> Option<Session> {
    match (outer, nested) {
        (_, Some(nested)) if nested.clients == 0 => Some(nested),
        (Some(outer), Some(nested)) => Some(Session {
            // One client of the nested session is the place's own.
            clients: outer.clients + nested.clients.saturating_sub(1),
            ..outer
        }),
        (Some(outer), None) => Some(Session {
            place: Origin {
                nested: None,
                ..outer.place.clone()
            },
            ..outer
        }),
        (None, nested) => nested,
    }
}

/// Mark every one of `sessions` found at the place of a stopped agent, which
/// another record of that place, such as the one Shoal created for the
/// agent, may have found too, with why it stopped and its execution. A
/// session with an execution of its own keeps its own state, and another
/// place keeps its own however its host names it, since a host may give
/// several places one name.
fn mark_stopped(sessions: &mut [Session]) {
    let stopped: Vec<_> = sessions
        .iter()
        .filter_map(|session| {
            Some((
                session.place.clone(),
                session.stopped.clone()?,
                session.execution.clone(),
            ))
        })
        .collect();
    for session in sessions
        .iter_mut()
        .filter(|session| session.execution.is_none())
    {
        if let Some((_, protection, execution)) = stopped
            .iter()
            .find(|(place, ..)| one_place(place, &session.place))
        {
            session.stopped = Some(protection.clone());
            session.execution = execution.clone();
        }
    }
}

/// Whether `a` and `b` name one place, whichever instance of it each
/// record found.
fn one_place(a: &Origin, b: &Origin) -> bool {
    let any_instance = |place: &Origin| Origin {
        instance: None,
        ..place.clone()
    };
    any_instance(a).same_place(&any_instance(b))
}

/// The session open at each of `places`, in order, asking each host that
/// `sessions_of` names about all of its places at once; a place without one
/// has none. A host that cannot answer leaves its places without one and
/// gives the last such failure, while the others' answers stand.
async fn open_at(
    places: &[&Origin],
    sessions_of: impl Fn(HostName) -> Option<&'static dyn Sessions>,
) -> (Vec<Option<Session>>, Option<anyhow::Error>) {
    let mut found = vec![None; places.len()];
    let mut failure = None;
    let mut hosts = Vec::new();
    for place in places {
        if !hosts.contains(&place.host) {
            hosts.push(place.host);
        }
    }
    for host in hosts {
        let Some(sessions) = sessions_of(host) else {
            continue;
        };
        let (indices, group): (Vec<usize>, Vec<&Origin>) = places
            .iter()
            .enumerate()
            .filter(|(_, place)| place.host == host)
            .map(|(index, place)| (index, *place))
            .unzip();
        match sessions.sessions_at(&group).await {
            Ok(sessions) => {
                for (index, session) in indices.into_iter().zip(sessions) {
                    found[index] = session;
                }
            }
            Err(error) => failure = Some(error),
        }
    }
    (found, failure)
}

/// `places` without repeats of a place: the same ID in the same host, its
/// data, which tells servers of one host apart, and its instance, which tells
/// a later place under a reused ID apart. A place keeps the agent any of its
/// repeats records there and the session one of them nests there, and a
/// session another of them nests is that one's.
fn distinct_places<'a>(places: &[Recorded<'a>]) -> Vec<Recorded<'a>> {
    let key = |place: &'a Origin| (place.host, place.id(), &place.data, &place.instance);
    let nested: Vec<_> = (places.iter())
        .filter_map(|recorded| recorded.place.nested.as_deref())
        .map(key)
        .collect();
    let mut distinct: Vec<Recorded<'a>> = Vec::new();
    for recorded in places
        .iter()
        .filter(|recorded| !nested.contains(&key(recorded.place)))
    {
        match distinct
            .iter_mut()
            .find(|kept| key(kept.place) == key(recorded.place))
        {
            Some(kept) => {
                if kept.place.nested.is_none() && recorded.place.nested.is_some() {
                    kept.place = recorded.place;
                }
                kept.agent = kept.agent.or(recorded.agent);
                kept.execution = kept.execution.or(recorded.execution);
                kept.stopped = kept.stopped.or(recorded.stopped);
            }
            None => distinct.push(*recorded),
        }
    }
    distinct
}

/// What the places `agents` were recorded running in, and `owned`, the
/// places Shoal created for spaces, tell of agent sessions before any host
/// is asked.
#[derive(Debug, PartialEq, Eq)]
pub struct KnownSessions<'a> {
    /// The spaces with a connected agent in a host that keeps sessions,
    /// including one Shoal stopped whose wrapper waits to restore it,
    /// which attaching resumes.
    pub owners: HashSet<SpaceId>,
    /// The places of the other spaces, of executions whose wrapper is gone
    /// and those Shoal created, such as a session left at its shell after a
    /// failed launch, whose hosts [`open_session_owners`] asks once the
    /// caller has their owners' settings.
    pub to_ask: Vec<(&'a SpaceId, &'a Origin)>,
}

/// What the places of `agents` and `owned` tell of agent sessions; an
/// agent without a recorded place has none.
pub fn known_sessions<'a>(
    agents: &'a [RunningAgent],
    owned: impl IntoIterator<Item = (&'a SpaceId, &'a Origin)>,
) -> KnownSessions<'a> {
    let places = agent_places(agents);
    let owners = session_owners(&places.connected);
    let to_ask = (places.unknown.into_iter())
        .chain(owned)
        .filter(|(owner, _)| !owners.contains(*owner))
        .collect();
    KnownSessions { owners, to_ask }
}

/// Where agents were recorded running, as (space ID, place) pairs.
#[derive(Debug, Default, PartialEq, Eq)]
struct AgentPlaces<'a> {
    /// Connected agents, which run in their places or, stopped by Shoal,
    /// wait there to be restored.
    connected: Vec<(&'a SpaceId, &'a Origin)>,
    /// Executions whose wrapper is gone, whose record outlives the session.
    unknown: Vec<(&'a SpaceId, &'a Origin)>,
}

/// Sort the places of `agents` by what runs in them; an agent without a
/// recorded place has none.
fn agent_places(agents: &[RunningAgent]) -> AgentPlaces<'_> {
    let mut places = AgentPlaces::default();
    for agent in agents {
        let Some(place) = agent.origin.as_ref() else {
            continue;
        };
        let kind = match &agent.agent {
            None => &mut places.unknown,
            Some(_) => &mut places.connected,
        };
        kind.push((&agent.space_id, place));
    }
    places
}

/// The IDs of the owners with an agent session: those of `places`, recorded
/// for owners by ID, whose hosts keep sessions.
pub fn session_owners(places: &[(&SpaceId, &Origin)]) -> HashSet<SpaceId> {
    places
        .iter()
        .filter(|(_, place)| named(place.host).sessions().is_some())
        .map(|(owner, _)| (*owner).clone())
        .collect()
}

/// The IDs of the owners of `places`, recorded for owners by ID, with a
/// session still open there, asking only hosts the owner's `settings`
/// enable. A place whose owner has no settings, or whose host cannot say,
/// counts as closed.
pub async fn open_session_owners(
    places: &[(&SpaceId, &Origin)],
    settings: &HashMap<SpaceId, Effective>,
) -> HashSet<SpaceId> {
    let places = enabled_places(places, settings);
    let origins: Vec<&Origin> = places.iter().map(|(_, place)| *place).collect();
    let (found, _) = open_at(&origins, |host| named(host).sessions()).await;
    places
        .iter()
        .zip(found)
        .filter(|(_, session)| session.is_some())
        .map(|((owner, _), _)| (*owner).clone())
        .collect()
}

/// Those of `places`, recorded for owners by ID, in a host their owner's
/// `settings` enable.
fn enabled_places<'a>(
    places: &[(&'a SpaceId, &'a Origin)],
    settings: &HashMap<SpaceId, Effective>,
) -> Vec<(&'a SpaceId, &'a Origin)> {
    places
        .iter()
        .filter(|(owner, place)| {
            settings
                .get(*owner)
                .is_some_and(|settings| enabled_host(settings, place.host).is_some())
        })
        .copied()
        .collect()
}

/// Show `session` in this terminal through the host that keeps it, then
/// say how to return to it with `shoal attach <space>` when it outlives the
/// attach. Returns whether its place still has a session once shown.
pub async fn attach(space: &str, session: &Session) -> Result<bool> {
    let itself = shown_itself_here(&session.place);
    let (place, name) = shown(session, shown_place(&session.place, itself));
    let (places, sessions) = keeping(place)?;
    places.show(place, name).await?;
    report_detach(sessions, space, name).await?;
    Ok(still_open(sessions, place).await)
}

/// Show `session` to `viewer`, a user on another machine, through the host
/// that keeps it, then say how to return to it as [`attach`] does. Returns
/// whether the session still runs once the viewer left it.
pub async fn show_elsewhere(
    space: &str,
    session: &Session,
    viewer: &Elsewhere<'_>,
) -> Result<bool> {
    let itself = shown_itself_elsewhere(viewer);
    let (place, name) = shown(session, shown_place(&session.place, itself));
    let (places, sessions) = keeping(place)?;
    places.show_elsewhere(place, name, viewer).await?;
    report_detach(sessions, space, name).await?;
    Ok(still_open(sessions, place).await)
}

/// `shown`, `session`'s place or the session nested in it, with the name
/// its host knows it by.
fn shown<'a>(session: &'a Session, shown: &'a Origin) -> (&'a Origin, &'a str) {
    match std::ptr::eq(shown, &session.place) {
        true => (shown, &session.name),
        false => (shown, shown.id()),
    }
}

/// What shows `place`, a place recorded for work Shoal runs, on this
/// terminal: the place itself where its host serves the terminal and it
/// still shows the session nested in it, otherwise that session, which any
/// terminal attaches to.
pub async fn shown_here(place: &Origin) -> &Origin {
    let itself = shown_itself_here(place) && shows_nested(place).await;
    shown_place(place, itself)
}

/// What shows `place` to `viewer`, a user on another machine: the place
/// itself where their app for its host shows it and it still shows the
/// session nested in it, otherwise that session.
pub async fn shown_elsewhere<'a>(place: &'a Origin, viewer: &Elsewhere<'_>) -> &'a Origin {
    let itself = shown_itself_elsewhere(viewer) && shows_nested(place).await;
    shown_place(place, itself)
}

/// Whether this terminal shows a place of `place`'s host itself: where the
/// host serves it.
fn shown_itself_here(place: &Origin) -> bool {
    named(place.host).serves()
}

/// Whether `viewer` sees a place itself: where their app for its host
/// shows it.
fn shown_itself_elsewhere(viewer: &Elsewhere<'_>) -> bool {
    viewer.app.is_some()
}

/// Whether `place` still shows the session nested in it, if any: until a
/// detach there leaves the session without clients, the place's own client
/// among them. A session its host cannot read counts as shown.
async fn shows_nested(place: &Origin) -> bool {
    let Some(nested) = place.nested.as_deref() else {
        return true;
    };
    let Some(sessions) = named(nested.host).sessions() else {
        return true;
    };
    match sessions.session_at(nested).await {
        Ok(found) => still_shown(found.as_ref()),
        Err(_) => true,
    }
}

/// Whether a place shows `found`, the session nested in it as its host
/// found it: while it has a client, or once it is gone, when the place
/// shows what is left.
fn still_shown(found: Option<&Session>) -> bool {
    found.is_none_or(|session| session.clients > 0)
}

/// `place` where `itself`, otherwise the session nested in it, if any.
fn shown_place(place: &Origin, itself: bool) -> &Origin {
    match &place.nested {
        Some(nested) if !itself => nested,
        _ => place,
    }
}

/// The host that keeps the session at `place`, as it shows and reads its
/// sessions.
fn keeping(place: &Origin) -> Result<(&'static dyn Places, &'static dyn Sessions)> {
    let host = named(place.host);
    match (host.places(), host.sessions()) {
        (Some(places), Some(sessions)) => Ok((places, sessions)),
        _ => anyhow::bail!("{} keeps no sessions", host.name()),
    }
}

/// Say how to return to `session`, one `host` attached this terminal to,
/// with `shoal attach <space>` when it still runs after this terminal left
/// it, and whether it does.
pub async fn report_detach(host: &dyn Sessions, space: &str, session: &str) -> Result<bool> {
    let running = host.left_running(session).await?;
    if running {
        eprintln!("shoal: detached; run `shoal attach {space}` to return");
    }
    Ok(running)
}

/// Whether `place`, which its host showed here, still has a session there.
/// A host that keeps no sessions has none.
pub async fn left_open(place: &Origin) -> bool {
    match named(place.host).sessions() {
        Some(sessions) => still_open(sessions, place).await,
        None => false,
    }
}

/// Whether `place` has a session in `host`, which may have attached this
/// terminal until a detach or shown the place elsewhere. A failed lookup
/// counts as closed.
async fn still_open(host: &dyn Sessions, place: &Origin) -> bool {
    host.sessions_at(&[place])
        .await
        .is_ok_and(|found| found.into_iter().flatten().next().is_some())
}

#[cfg(test)]
mod tests;
