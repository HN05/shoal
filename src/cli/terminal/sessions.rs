//! The agent sessions hosts keep: finding them where the daemon recorded a
//! workspace's or chat's agents, and attaching this terminal to one.
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde::Serialize;

use super::{
    host::{Host, Sessions},
    hosts::{enabled_host, named},
};
use crate::{
    config::Effective,
    model::{Chat, Inspection},
    sessions::{HostName, Origin},
};

/// What an agent session runs for, by ID; a host that keeps ownership in its
/// sessions labels them with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum Owner<'a> {
    Workspace(&'a str),
    Chat(&'a str),
}

/// What kind of [`Owner`] a session has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum OwnerKind {
    Workspace,
    Chat,
}

impl<'a> Owner<'a> {
    pub fn kind(self) -> OwnerKind {
        match self {
            Self::Workspace(_) => OwnerKind::Workspace,
            Self::Chat(_) => OwnerKind::Chat,
        }
    }

    pub fn id(self) -> &'a str {
        match self {
            Self::Workspace(id) | Self::Chat(id) => id,
        }
    }
}

/// A running agent session that `shoal attach` can reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Session {
    pub name: String,
    /// The agent the daemon recorded running there, when one runs there.
    pub agent: Option<String>,
    /// Terminals attached to the session; a host that cannot count them
    /// gives one while it shows the session.
    pub clients: u32,
    /// Where it was recorded, which names the host keeping it.
    #[serde(skip)]
    place: Origin,
}

impl Session {
    /// The session `name` a host found open at `place`, with `clients`
    /// attached; the daemon's records supply its agent.
    pub(super) fn open(name: String, clients: u32, place: &Origin) -> Self {
        Self {
            name,
            agent: None,
            clients,
            place: place.clone(),
        }
    }

    /// The name, agent, and whether a terminal is attached.
    pub fn label(&self) -> String {
        let mut label = self.name.clone();
        if let Some(agent) = &self.agent {
            label.push_str(&format!("  {agent}"));
        }
        if self.clients > 0 {
            label.push_str("  attached");
        }
        label
    }
}

/// A place the daemon recorded, with the agent it recorded running there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) struct Recorded<'a> {
    pub place: &'a Origin,
    pub agent: Option<&'a str>,
}

impl<'a> Recorded<'a> {
    /// Where `inspection` recorded its workspace's executions running, with
    /// their agents, and the places Shoal created for it.
    pub fn workspace(inspection: &'a Inspection) -> Vec<Self> {
        let executions = inspection.executions.iter().filter_map(|execution| {
            Some(Self {
                place: execution.origin.as_ref()?,
                agent: execution.agent.as_deref(),
            })
        });
        let created = inspection
            .places
            .iter()
            .map(|place| Self { place, agent: None });
        executions.chain(created).collect()
    }

    /// Where `chat` was recorded running, with its agent.
    pub fn chat(chat: &'a Chat) -> Vec<Self> {
        chat.origin
            .iter()
            .map(|place| Self {
                place,
                agent: Some(&chat.agent),
            })
            .collect()
    }
}

/// A place where an agent runs, with the host that tells whether a terminal
/// shows it.
#[derive(Clone)]
pub(crate) struct Attachment {
    sessions: &'static dyn Sessions,
    place: Origin,
}

impl Attachment {
    /// What reads whether a terminal shows `place`, unless its host is
    /// disabled in `settings` or keeps no sessions.
    pub(crate) fn of(settings: &Effective, place: Origin) -> Option<Self> {
        let sessions = enabled_host(settings, place.host).and_then(Host::sessions)?;
        Some(Self { sessions, place })
    }

    /// Whether a terminal shows the place; one that is gone shows nothing.
    pub(crate) async fn attached(&self) -> Result<bool> {
        let session = self.sessions.session_at(&self.place).await?;
        Ok(session.is_some_and(|session| session.clients > 0))
    }
}

/// The agent sessions still open at `places` in the hosts `settings`
/// enable, where the daemon recorded a workspace's executions or a chat
/// running, or the places Shoal created for a workspace.
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
    Ok(places
        .iter()
        .zip(found)
        .filter_map(|(recorded, session)| {
            Some(Session {
                agent: recorded.agent.map(str::to_owned),
                ..session?
            })
        })
        .collect())
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
            Some(kept) => kept.agent = kept.agent.or(recorded.agent),
            None => distinct.push(*recorded),
        }
    }
    distinct
}

/// The IDs of the owners with an agent session: those of `places`, recorded
/// for owners by ID, whose hosts keep sessions.
pub(in crate::cli) fn session_owners(places: &[(&str, &Origin)]) -> HashSet<String> {
    places
        .iter()
        .filter(|(_, place)| named(place.host).sessions().is_some())
        .map(|(owner, _)| (*owner).to_owned())
        .collect()
}

/// The IDs of the owners of `places`, recorded for owners by ID, with a
/// session still open there, asking only hosts the owner's `settings`
/// enable. A place whose owner has no settings, or whose host cannot say,
/// counts as closed.
pub(in crate::cli) async fn open_session_owners(
    places: &[(&str, &Origin)],
    settings: &HashMap<String, Effective>,
) -> HashSet<String> {
    let places: Vec<_> = places
        .iter()
        .filter(|(owner, place)| {
            settings
                .get(*owner)
                .is_some_and(|settings| enabled_host(settings, place.host).is_some())
        })
        .collect();
    let origins: Vec<&Origin> = places.iter().map(|(_, place)| *place).collect();
    let (found, _) = open_at(&origins, |host| named(host).sessions()).await;
    places
        .iter()
        .zip(found)
        .filter(|(_, session)| session.is_some())
        .map(|((owner, _), _)| (*owner).to_owned())
        .collect()
}

/// Show `session` in this terminal through the host that keeps it, then
/// say how to return to it by `name` when it outlives the attach. Returns
/// whether its place still has a session once shown.
pub(in crate::cli) async fn attach(name: &str, session: &Session) -> Result<bool> {
    let host = named(session.place.host);
    let (Some(places), Some(sessions)) = (host.places(), host.sessions()) else {
        anyhow::bail!("{} keeps no sessions", host.name());
    };
    places.show(&session.place, &session.name).await?;
    report_detach(sessions, name, &session.name).await?;
    Ok(still_open(sessions, &session.place).await)
}

/// Say how to return to `session`, one `host` attached this terminal to, by
/// `name` when it still runs after this terminal left it.
pub(super) async fn report_detach(host: &dyn Sessions, name: &str, session: &str) -> Result<()> {
    if host.left_running(session).await? {
        eprintln!("shoal: detached from {session}; run `shoal attach {name}` to return");
    }
    Ok(())
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
