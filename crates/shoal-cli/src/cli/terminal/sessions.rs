//! The agent sessions hosts keep: finding them where the daemon recorded a
//! workspace's or chat's agents, and attaching this terminal to one.
use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::{
    config::Effective,
    model::{Chat, Execution},
    protocol::Protection,
    sessions::{HostName, Origin},
    terminal::{Host, Session, Sessions, enabled_host, named},
};

/// A place the daemon recorded, with the agent it recorded running there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) struct Recorded<'a> {
    pub place: &'a Origin,
    pub agent: Option<&'a crate::agent::Agent>,
    /// The execution recorded there.
    pub execution: Option<&'a ExecutionId>,
    /// Why Shoal stopped that agent, while its wrapper waits to restore it.
    pub stopped: Option<&'a Protection>,
}

impl<'a> Recorded<'a> {
    /// Where a workspace's `executions` were recorded running, with their
    /// agents, and the `places` Shoal created for it.
    pub fn workspace(executions: &'a [Execution], places: &'a [Origin]) -> Vec<Self> {
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

    /// Where `chat` was recorded running, with its agent.
    pub fn chat(chat: &'a Chat) -> Vec<Self> {
        chat.origin
            .iter()
            .map(|place| Self {
                place,
                agent: Some(&chat.agent),
                execution: None,
                stopped: None,
            })
            .collect()
    }
}

/// The agent sessions still open at `places` in the hosts `settings`
/// enable, where the daemon recorded a workspace's executions or a chat
/// running, or the places Shoal created for a workspace. A session where a
/// stopped agent waits to be restored says so, whichever place found it.
pub(in crate::cli) async fn sessions(
    settings: &Effective,
    places: &[Recorded<'_>],
) -> Result<Vec<Session>> {
    let places = distinct_places(places);
    let origins: Vec<_> = places.iter().map(|recorded| recorded.place).collect();
    let (found, failure) = open_at(&origins, |host| {
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
/// repeats records there.
fn distinct_places<'a>(places: &[Recorded<'a>]) -> Vec<Recorded<'a>> {
    let key = |place: &'a Origin| (place.host, place.id(), &place.data, &place.instance);
    let mut distinct: Vec<Recorded<'a>> = Vec::new();
    for recorded in places {
        match distinct
            .iter_mut()
            .find(|kept| key(kept.place) == key(recorded.place))
        {
            Some(kept) => {
                kept.agent = kept.agent.or(recorded.agent);
                kept.execution = kept.execution.or(recorded.execution);
                kept.stopped = kept.stopped.or(recorded.stopped);
            }
            None => distinct.push(*recorded),
        }
    }
    distinct
}

/// The IDs of the owners with an agent session: those of `places`, recorded
/// for owners by ID, whose hosts keep sessions.
pub(in crate::cli) fn session_owners(places: &[(&SpaceId, &Origin)]) -> HashSet<SpaceId> {
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
pub(in crate::cli) async fn open_session_owners(
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
pub(in crate::cli) async fn attach(space: &str, session: &Session) -> Result<bool> {
    let host = named(session.place.host);
    let (Some(places), Some(sessions)) = (host.places(), host.sessions()) else {
        anyhow::bail!("{} keeps no sessions", host.name());
    };
    places.show(&session.place, &session.name).await?;
    report_detach(sessions, space, &session.name).await?;
    Ok(still_open(sessions, &session.place).await)
}

/// Say how to return to `session`, one `host` attached this terminal to,
/// with `shoal attach <space>` when it still runs after this terminal left
/// it, and whether it does.
pub(super) async fn report_detach(host: &dyn Sessions, space: &str, session: &str) -> Result<bool> {
    let running = host.left_running(session).await?;
    if running {
        eprintln!("shoal: detached from {session}; run `shoal attach {space}` to return");
    }
    Ok(running)
}

/// Whether `place`, which its host showed here, still has a session there.
/// A host that keeps no sessions has none.
pub(super) async fn left_open(place: &Origin) -> bool {
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
