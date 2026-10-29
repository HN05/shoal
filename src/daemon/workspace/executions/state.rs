//! What the daemon keeps in memory about executions while their wrappers are
//! connected: the channel that stops each, the agents they run, why the
//! daemon asked them to stop, how far each reported exit has been recorded,
//! and the scope tokens that act for them.
//!
//! Locks are taken in the order connections, agents, stop requests. Agents
//! and stop requests are reached only through closures, which cannot await
//! another lock, or through [`Connections`], which already holds the first.
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Mutex as StdMutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, Result, ensure};
use tokio::sync::{Mutex, MutexGuard, watch};

use super::{ExecutionKind, StopRequest};
use crate::daemon::{
    scope::{Caller, ScopedExecution},
    workspace::agents::Agent,
};

/// Launches that ended unrun the daemon remembers; a start asks about its
/// own within moments.
const UNRUN_LAUNCHES: usize = 64;

/// The step a connected execution's completion is on after its wrapper
/// reported the exit, named when a stop times out waiting for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Completing {
    /// Waiting for the daemon to start recording the completion.
    Reported,
    /// Stopping the processes an exited agent left behind.
    StoppingSurvivors,
    /// Scanning for processes the execution left behind.
    Verifying,
}

impl std::fmt::Display for Completing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Reported => "command exited, completion not recorded yet",
            Self::StoppingSurvivors => "command exited, stopping its remaining processes",
            Self::Verifying => "command exited, verifying that its processes stopped",
        })
    }
}

#[derive(Default)]
pub(crate) struct Executions {
    /// Connected executions and the channel that asks their wrapper to stop.
    connections: Mutex<HashMap<String, watch::Sender<bool>>>,
    agents: Mutex<HashMap<String, Agent>>,
    /// Why the daemon asked connected executions to stop.
    stop_requests: Mutex<HashMap<String, StopRequest>>,
    /// Connected executions whose wrapper stops instead of reattaching when
    /// the daemon shuts down.
    unattachable: StdMutex<HashSet<String>>,
    /// Connected executions whose wrapper reported its exit, by the
    /// completion step the daemon is on.
    completing: StdMutex<HashMap<String, Completing>>,
    /// Recent demo launches whose execution ended before their command ran,
    /// for the start waiting on each.
    unrun_demo_launches: StdMutex<VecDeque<String>>,
    /// Set once the daemon refuses new registrations.
    closed: AtomicBool,
    shutting_down: AtomicBool,
    /// Scope token → the execution it was issued to.
    scopes: Mutex<HashMap<String, Caller>>,
}

impl Executions {
    /// Take the connections lock. Registration and stops hold it across their
    /// store work, so an execution registers either before a stop lists it or
    /// after the stop's request reached it; external commands and agent
    /// lifetimes run without it.
    pub(crate) async fn connections(&self) -> Connections<'_> {
        Connections {
            executions: self,
            senders: self.connections.lock().await,
        }
    }

    pub(crate) async fn is_connected(&self, id: &str) -> bool {
        self.connections().await.is_connected(id)
    }

    /// Every execution with a connection, including one whose wrapper is
    /// going.
    pub(crate) async fn connection_ids(&self) -> HashSet<String> {
        self.connections.lock().await.keys().cloned().collect()
    }

    pub(crate) async fn has_connections(&self) -> bool {
        !self.connections.lock().await.is_empty()
    }

    /// Drop the connection of `id` and what the daemon kept for it, returning
    /// whether it ran an agent.
    pub(crate) async fn disconnect(&self, id: &str) -> bool {
        self.connections().await.disconnect(id).await
    }

    /// Refuse new registrations, as the daemon exits. Taking the connections
    /// lock lets a registration under way finish first.
    pub(crate) async fn close(&self) {
        let _connections = self.connections().await;
        self.closed.store(true, Ordering::Relaxed);
    }

    /// Before the daemon exits, refuse new registrations and ask every
    /// connected execution to stop for `request`.
    pub(crate) async fn shut_down(&self, request: StopRequest) {
        let connections = self.connections().await;
        self.closed.store(true, Ordering::Relaxed);
        self.shutting_down.store(true, Ordering::Relaxed);
        for id in connections.senders.keys() {
            let _ = connections.stop(id, &request, false).await;
        }
    }

    /// Set once shutdown starts asking executions to stop.
    pub(crate) fn shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Relaxed)
    }

    /// Why the daemon asked a connected execution to stop, if it did.
    pub(crate) async fn stop_request(&self, id: &str) -> Option<StopRequest> {
        self.stop_requests.lock().await.get(id).cloned()
    }

    /// Run `f` on the tracked agent of `id`, if there is one.
    pub(crate) async fn with_agent<R>(
        &self,
        id: &str,
        f: impl FnOnce(&mut Agent) -> R,
    ) -> Option<R> {
        self.agents.lock().await.get_mut(id).map(f)
    }

    /// Run `f` on every tracked agent, by execution.
    pub(crate) async fn with_agents<R>(
        &self,
        f: impl FnOnce(&mut HashMap<String, Agent>) -> R,
    ) -> R {
        f(&mut *self.agents.lock().await)
    }

    /// Record that a restart would stop this connected execution.
    pub(crate) fn mark_unattachable(&self, id: &str) {
        self.unattachable
            .lock()
            .expect("unattachable executions lock")
            .insert(id.to_owned());
    }

    /// Whether a restart would stop a connected execution. Disconnected
    /// records keep their state across it.
    pub(crate) fn has_unattachable(&self) -> bool {
        !self
            .unattachable
            .lock()
            .expect("unattachable executions lock")
            .is_empty()
    }

    /// Record the completion step a connected execution is on.
    pub(crate) fn set_completing(&self, id: &str, step: Completing) {
        self.completing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_owned(), step);
    }

    /// The completion step of a connected execution whose wrapper reported
    /// its exit.
    pub(crate) fn completing(&self, id: &str) -> Option<Completing> {
        self.completing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .copied()
    }

    /// Remember that `launch` ended before its command ran.
    pub(crate) fn remember_unrun_launch(&self, launch: String) {
        let mut launches = self
            .unrun_demo_launches
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if launches.len() == UNRUN_LAUNCHES {
            launches.pop_front();
        }
        launches.push_back(launch);
    }

    pub(crate) fn launch_ended_unrun(&self, launch: &str) -> bool {
        self.unrun_demo_launches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|ended| ended == launch)
    }

    /// Bind a scope token to the execution it was issued to.
    pub(crate) async fn issue_scope(&self, token: String, caller: Caller) {
        self.scopes.lock().await.insert(token, caller);
    }

    /// The execution a scope token was issued to, while it is connected.
    pub(crate) async fn scope(&self, token: &str) -> Option<Caller> {
        self.scopes.lock().await.get(token).cloned()
    }

    /// Forget the scope tokens of a workspace that no longer exists.
    pub(crate) async fn forget_workspace(&self, workspace_id: &str) {
        self.scopes
            .lock()
            .await
            .retain(|_, caller| caller.workspace_id != workspace_id);
    }

    /// The scope tokens issued, for tests that pause registration.
    #[cfg(test)]
    pub(crate) async fn scopes(&self) -> MutexGuard<'_, HashMap<String, Caller>> {
        self.scopes.lock().await
    }

    /// Whether something holds the connections lock.
    #[cfg(test)]
    pub(crate) fn connections_locked(&self) -> bool {
        self.connections.try_lock().is_err()
    }
}

/// The connections lock, held while registering or stopping executions.
pub(crate) struct Connections<'a> {
    executions: &'a Executions,
    senders: MutexGuard<'a, HashMap<String, watch::Sender<bool>>>,
}

impl Connections<'_> {
    /// Refuse a registration once the daemon closed registrations.
    pub(crate) fn ensure_accepting(&self) -> Result<()> {
        ensure!(
            !self.executions.closed.load(Ordering::Relaxed),
            "the daemon is shutting down; retry once it restarts"
        );
        Ok(())
    }

    /// Whether the wrapper of `id` is connected and listening.
    pub(crate) fn is_connected(&self, id: &str) -> bool {
        self.senders
            .get(id)
            .is_some_and(|sender| !sender.is_closed())
    }

    /// The channel that stops the wrapper of `id`.
    pub(crate) fn stop_channel(&self, id: &str) -> Option<&watch::Sender<bool>> {
        self.senders.get(id)
    }

    /// Give a wrapper its stop channel, starting as `stopped`, and let its
    /// scope token act for the execution.
    pub(crate) async fn connect(
        &mut self,
        id: &str,
        kind: ExecutionKind,
        workspace_id: &str,
        scope_token: String,
        stopped: bool,
    ) -> watch::Receiver<bool> {
        let (sender, stop) = watch::channel(stopped);
        self.senders.insert(id.to_owned(), sender);
        self.executions
            .issue_scope(
                scope_token,
                Caller {
                    execution: Some(ScopedExecution {
                        id: id.to_owned(),
                        kind,
                    }),
                    workspace_id: workspace_id.to_owned(),
                },
            )
            .await;
        stop
    }

    /// Ask the connected wrapper of `id` to stop for `request`, cancelling
    /// its agent's automatic restore. With `spare_protected`, an agent that
    /// protection stopped keeps waiting for its restore instead; returns
    /// whether the wrapper was asked.
    pub(crate) async fn stop(
        &self,
        id: &str,
        request: &StopRequest,
        spare_protected: bool,
    ) -> Result<bool> {
        let Some(sender) = self.senders.get(id).filter(|sender| !sender.is_closed()) else {
            return Ok(false);
        };
        let spared = self
            .executions
            .with_agent(id, |agent| {
                if spare_protected && agent.protected() {
                    return true;
                }
                agent.cancel_recovery();
                false
            })
            .await;
        if spared == Some(true) {
            return Ok(false);
        }
        self.executions
            .stop_requests
            .lock()
            .await
            .insert(id.to_owned(), request.clone());
        sender
            .send(true)
            .context("execution disconnected during stop")?;
        Ok(true)
    }

    /// Run `f` on every tracked agent, by execution.
    pub(crate) async fn with_agents<R>(
        &self,
        f: impl FnOnce(&mut HashMap<String, Agent>) -> R,
    ) -> R {
        self.executions.with_agents(f).await
    }

    /// Drop the connection of `id`, then, without the lock, what the daemon
    /// kept for it, returning whether it ran an agent.
    pub(crate) async fn disconnect(mut self, id: &str) -> bool {
        self.senders.remove(id);
        let executions = self.executions;
        drop(self);
        executions
            .unattachable
            .lock()
            .expect("unattachable executions lock")
            .remove(id);
        executions
            .completing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        let agent = executions.agents.lock().await.remove(id).is_some();
        executions.stop_requests.lock().await.remove(id);
        executions
            .scopes
            .lock()
            .await
            .retain(|_, caller| caller.execution_id() != Some(id));
        agent
    }
}
