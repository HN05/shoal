//! Which sessions of a space a handoff copies into its attached files, and
//! how it lists them for the new tool: every session the space's agents
//! reported, copied again only when its files changed since an earlier
//! handoff copied them, and those whose files are not here named as
//! missing.
use crate::Spaces;
use crate::daemon::Daemon;
use std::{collections::BTreeMap, path::Path, path::PathBuf};

use anyhow::Result;
use shoal_ids::agent_session_ids::AgentSessionId;

use crate::{
    agent::Agent,
    workspace_files::{AttachedFile, FileSource, NewFile, SessionCopy},
};
use shoal_protocol::integrations::IntegrationKind;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::transcripts::{self, Environments, ReportedSession, SpaceSessions};

/// A session a handoff lists for the new tool: the copy of its
/// conversation and how many other files of it were copied, or why its
/// files are not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub agent: Agent,
    pub session: AgentSessionId,
    pub integration: IntegrationKind,
    pub transcript: Option<PathBuf>,
    pub others: usize,
    pub missing: Option<String>,
}

/// One file of a session as its tool keeps it now: its path in the tool's
/// session directory, its path here and its length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Present {
    pub relative: PathBuf,
    pub path: PathBuf,
    pub bytes: u64,
    pub transcript: bool,
}

/// A session's files as its tool keeps them now, or why they are not here.
#[derive(Debug)]
enum Located {
    Here(Vec<Present>),
    Missing(String),
}

impl Spaces {
    /// The sessions of `space` a handoff lists, oldest first, once the ones
    /// whose files changed since a handoff copied them are copied again.
    pub(crate) async fn copy_sessions(
        &self,
        daemon: &dyn Daemon,
        space: &crate::model::Space,
    ) -> Result<Vec<Listed>> {
        let selector = SpaceSelector::id(&space.id);
        let sessions = self.space_sessions(daemon, &selector).await?;
        let home = self.paths.home.clone();
        let located = tokio::task::spawn_blocking(move || locate(&home, &sessions)).await?;
        let earlier = self.attached_files(&selector).await?;
        let mut missing = Vec::new();
        for (reported, integration, located) in located {
            match located {
                Located::Here(files) => {
                    if changed_since_copied(&reported.session, &files, &earlier) {
                        let copies = session_copies(&reported, integration, files);
                        (self.attach_session_files(daemon, &space.clone().into(), copies)).await?;
                    }
                }
                Located::Missing(why) => missing.push(Listed {
                    agent: reported.agent,
                    session: reported.session,
                    integration,
                    transcript: None,
                    others: 0,
                    missing: Some(why),
                }),
            }
        }
        let copied = self.attached_files(&selector).await?;
        Ok(listing(&copied, missing))
    }
}

/// The reported `sessions` with their tools and where their files are,
/// those that run now last, as they are likely the newest.
fn locate(
    home: &Path,
    sessions: &SpaceSessions,
) -> Vec<(ReportedSession, IntegrationKind, Located)> {
    let environments = Environments::new(home, &sessions.environments);
    let environments = environments.each();
    (sessions.sessions.iter().rev())
        .map(|reported| {
            let integration = (sessions.integrations.get(&reported.agent).copied())
                .unwrap_or(IntegrationKind::None);
            let located = located(reported, integration, &environments);
            (reported.clone(), integration, located)
        })
        .collect()
}

/// Where the files of `reported`, kept as `integration`'s tool keeps them,
/// are in the first of `environments` that has them.
fn located(
    reported: &ReportedSession,
    integration: IntegrationKind,
    environments: &[transcripts::AgentEnvironment],
) -> Located {
    let Some(store) = transcripts::store(integration) else {
        return Located::Missing(format!(
            "Shoal does not know where {} keeps its sessions",
            reported.agent
        ));
    };
    match transcripts::find(store, environments, reported.session.as_str()) {
        Ok(Some(found)) => Located::Here(present(&found)),
        Ok(None) => Located::Missing("its files are not on this machine".to_owned()),
        Err(error) => Located::Missing(format!("its files could not be looked for: {error:#}")),
    }
}

/// The files `found` names, with their lengths now; one gone meanwhile
/// drops out.
fn present(found: &transcripts::Found) -> Vec<Present> {
    (found.files.files.iter())
        .filter_map(|relative| {
            let path = found.directory.join(relative);
            let bytes = std::fs::metadata(&path).ok()?.len();
            Some(Present {
                transcript: found.files.transcript.as_ref() == Some(relative),
                relative: relative.clone(),
                path,
                bytes,
            })
        })
        .collect()
}

/// Whether `files`, the files of `session` now, differ from what the
/// handoff copies among `attached` last copied of it: a file it never
/// copied, or one whose length changed.
pub fn changed_since_copied(
    session: &AgentSessionId,
    files: &[Present],
    attached: &[AttachedFile],
) -> bool {
    let copied = copied_files(attached);
    let Some(copied) = copied.get(session) else {
        return true;
    };
    (files.iter()).any(|file| {
        copied
            .get(&file.relative)
            .is_none_or(|record| record.bytes != file.bytes)
    })
}

/// The handoff copies among `attached` by session, then by the session
/// file each copies, the newest copy of each.
fn copied_files(
    attached: &[AttachedFile],
) -> BTreeMap<&AgentSessionId, BTreeMap<&PathBuf, &AttachedFile>> {
    let mut copied: BTreeMap<_, BTreeMap<_, _>> = BTreeMap::new();
    for file in attached
        .iter()
        .filter(|file| file.source == FileSource::Handoff)
    {
        if let Some(session) = &file.session {
            copied
                .entry(&session.session)
                .or_default()
                .insert(&session.file, file);
        }
    }
    copied
}

/// What attaches copies of `files`, the files of `reported`'s session that
/// `integration`'s tool keeps.
fn session_copies(
    reported: &ReportedSession,
    integration: IntegrationKind,
    files: Vec<Present>,
) -> Vec<(NewFile, SessionCopy)> {
    files
        .into_iter()
        .map(|file| {
            let title = format!(
                "{} session {}: {}",
                reported.agent,
                reported.session,
                file.relative.display()
            );
            let new = NewFile {
                path: file.path,
                title: Some(title),
            };
            let copy = SessionCopy {
                agent: reported.agent.clone(),
                session: reported.session.clone(),
                integration,
                file: file.relative,
                transcript: file.transcript,
            };
            (new, copy)
        })
        .collect()
}

/// The sessions the handoff copies among `attached` hold, in the order
/// they were first copied, each with the newest copy of its conversation,
/// then the `missing` ones that no copy holds.
pub fn listing(attached: &[AttachedFile], missing: Vec<Listed>) -> Vec<Listed> {
    let copied = copied_files(attached);
    let mut first_copied: Vec<(i64, Listed)> = copied
        .values()
        .filter_map(|files| {
            let first = files.values().map(|file| file.id).min()?;
            let any = files.values().next()?.session.as_ref()?;
            let transcript = (files.values())
                .find(|file| file.session.as_ref().is_some_and(|copy| copy.transcript))
                .and_then(|file| file.path().map(Path::to_path_buf));
            let others = files.len() - usize::from(transcript.is_some());
            let listed = Listed {
                agent: any.agent.clone(),
                session: any.session.clone(),
                integration: any.integration,
                transcript,
                others,
                missing: None,
            };
            Some((first, listed))
        })
        .collect();
    first_copied.sort_by_key(|(first, _)| *first);
    let missing = missing
        .into_iter()
        .filter(|listed| !copied.contains_key(&listed.session));
    (first_copied.into_iter().map(|(_, listed)| listed))
        .chain(missing)
        .collect()
}

#[cfg(test)]
mod tests;
