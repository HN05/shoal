//! What a process's own environment says about its owner: the markers of
//! Shoal's identity it carries, read once the environment has settled.
use super::{Identity, Image, Lineage, alive, lineage, platform};
use crate::client::execution_ids::ExecutionId;
use crate::protocol::timing;
use anyhow::Result;
use std::time::Duration;

/// One live process and what its own environment says about its owner.
pub(super) struct Observed {
    pub(super) lineage: Lineage,
    pub(super) evidence: Evidence,
}

/// The markers of Shoal's identity an environment carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Marks {
    pub(super) execution: Option<ExecutionId>,
    pub(super) workspace: Option<String>,
    pub(super) chat: Option<String>,
    /// The run directory of a one-shot agent run.
    pub(super) oneshot: Option<String>,
    /// It runs one of the hooks the daemon starts.
    pub(super) hook: bool,
}

/// The marker a scan attributes processes by.
#[derive(Clone, Copy)]
pub(super) enum Marker {
    Execution,
    Workspace,
    Chat,
    OneShot,
}

impl Marks {
    pub(super) fn read(environment: &[Vec<u8>]) -> Self {
        let value = |variable: &str| {
            let prefix = format!("{variable}=");
            environment
                .iter()
                .find_map(|entry| entry.strip_prefix(prefix.as_bytes()))
                .and_then(|value| std::str::from_utf8(value).ok())
                .map(str::to_owned)
        };
        Self {
            execution: value(crate::env::EXECUTION_ID).map(ExecutionId::from),
            workspace: value(crate::env::WORKSPACE_ID),
            chat: value(crate::env::CHAT),
            oneshot: value(crate::env::ONESHOT_RUN),
            hook: value(crate::env::HOOK).is_some(),
        }
    }

    pub(super) fn get(&self, marker: Marker) -> Option<&str> {
        match marker {
            Marker::Execution => self.execution.as_ref().map(ExecutionId::as_str),
            Marker::Workspace => self.workspace.as_deref(),
            // A workspace's process a chat started belongs to the workspace.
            Marker::Chat => self.chat.as_deref().filter(|_| !self.of_workspace()),
            Marker::OneShot => self.oneshot.as_deref(),
        }
    }

    /// Whether a workspace or one of its executions owns the process.
    pub(super) fn of_workspace(&self) -> bool {
        self.workspace.is_some() || self.execution.is_some()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.execution.is_none()
            && self.workspace.is_none()
            && self.chat.is_none()
            && self.oneshot.is_none()
    }
}

pub(super) enum Evidence {
    Marked(Marks),
    Unmarked,
    Unreadable,
    /// Settled empty, which a platform binary's hidden environment also reads as.
    Hidden,
    /// A setuid program this user started, whose environment came from the
    /// parent that started it. It runs as another user, so it is never
    /// reported or stopped itself.
    Elevated,
}

/// Whether `identity` was started with `variable` set. An environment that
/// cannot be read counts as without it.
pub(crate) fn started_with(identity: &Identity, variable: &str) -> bool {
    let prefix = format!("{variable}=");
    let deadline = std::time::Instant::now() + timing::PROCESS_SETTLE_TIMEOUT;
    matches!(
        visibility(identity, deadline),
        Ok(Visibility::Environment(environment))
            if environment.iter().any(|entry| entry.starts_with(prefix.as_bytes()))
    )
}

/// Read `pid`'s environment for Shoal's markers, unless it has exited.
pub(super) fn observe(pid: u32, deadline: std::time::Instant) -> Result<Option<Observed>> {
    observe_with(pid, deadline, platform::hides_environment)
}

/// `observe`, telling hidden environments with `hides`.
pub(super) fn observe_with(
    pid: u32,
    deadline: std::time::Instant,
    hides: impl FnOnce(u32) -> bool,
) -> Result<Option<Observed>> {
    let Some(lineage) = lineage(pid)? else {
        return Ok(None);
    };
    let evidence = match visibility(&lineage.identity, deadline)? {
        Visibility::Environment(environment) if environment.is_empty() && hides(pid) => {
            // A process that exited fails the check as if it hid its
            // environment.
            if !alive(&lineage.identity)? {
                return Ok(None);
            }
            Evidence::Hidden
        }
        Visibility::Environment(environment) => {
            let marks = Marks::read(&environment);
            if marks.is_empty() {
                Evidence::Unmarked
            } else {
                Evidence::Marked(marks)
            }
        }
        Visibility::Unreadable => Evidence::Unreadable,
        Visibility::Gone => return Ok(None),
    };
    Ok(Some(Observed { lineage, evidence }))
}

/// Observe `pid`, a setuid program this user started, unless it has exited.
pub(super) fn observe_elevated(pid: u32) -> Result<Option<Observed>> {
    Ok(platform::elevated_lineage(pid)?.map(|lineage| Observed {
        lineage,
        evidence: Evidence::Elevated,
    }))
}

pub(super) enum Visibility {
    Environment(Vec<Vec<u8>>),
    Unreadable,
    Gone,
}

/// Linux reads an empty environment while a process replaces its image and
/// after an exiting process releases its memory; neither is evidence about
/// ownership. Wait for loading images, then confirm a settled empty read.
pub(super) fn visibility(identity: &Identity, deadline: std::time::Instant) -> Result<Visibility> {
    visibility_with(identity, deadline, platform::environment)
}

/// `visibility`, reading environments with `environment`.
pub(super) fn visibility_with(
    identity: &Identity,
    deadline: std::time::Instant,
    mut environment: impl FnMut(u32) -> Result<Vec<Vec<u8>>>,
) -> Result<Visibility> {
    loop {
        let read = environment(identity.pid);
        if !alive(identity)? {
            return Ok(Visibility::Gone);
        }
        match read {
            Ok(environment) if !environment.is_empty() => {
                return Ok(Visibility::Environment(environment));
            }
            // Without CAP_SYS_PTRACE, Linux refuses an exiting process's
            // environment. A refusal also hides env_end, so never wait on it.
            Err(_) => match platform::image(identity.pid)? {
                Image::Ended => return Ok(Visibility::Gone),
                _ if platform::REFUSED_WHILE_LOADING && std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                _ => return Ok(Visibility::Unreadable),
            },
            Ok(_) => {}
        }
        match platform::image(identity.pid)? {
            Image::Ended => return Ok(Visibility::Gone),
            Image::Loading if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Image::Loading => return Ok(Visibility::Unreadable),
            // The image may have settled after the empty read.
            Image::Settled => {
                let read = environment(identity.pid);
                // The PID may have gone to another process since the read.
                if !alive(identity)? {
                    return Ok(Visibility::Gone);
                }
                match read {
                    Ok(environment) => return Ok(Visibility::Environment(environment)),
                    Err(_)
                        if platform::REFUSED_WHILE_LOADING
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(_) => return Ok(Visibility::Unreadable),
                }
            }
        }
    }
}

/// Whether the process `identity` names belongs to a workspace: it is one of
/// the execution `wrappers`, or its own environment, read rather than
/// hidden, carries a workspace's or an execution's marker.
pub fn owned_by_workspace(identity: &Identity, wrappers: &[Identity]) -> Result<bool> {
    if wrappers.contains(identity) {
        return Ok(true);
    }
    let deadline = std::time::Instant::now() + timing::PROCESS_SETTLE_TIMEOUT;
    Ok(observe(identity.pid, deadline)?.is_some_and(|observed| {
        observed.lineage.identity == *identity
            && matches!(&observed.evidence, Evidence::Marked(marks) if marks.of_workspace())
    }))
}

/// Whether the process's own environment, read rather than hidden, lacks
/// the execution's marker: a process started without it, such as a demo,
/// no longer belongs to the execution, nor do its descendants.
pub(super) fn left_execution(
    identity: &Identity,
    execution: &ExecutionId,
    deadline: std::time::Instant,
) -> Result<bool> {
    Ok(observe(identity.pid, deadline)?.is_some_and(|observed| {
        observed.lineage.identity == *identity
            && match &observed.evidence {
                Evidence::Unmarked => true,
                Evidence::Marked(marks) => marks.get(Marker::Execution) != Some(execution.as_str()),
                _ => false,
            }
    }))
}
