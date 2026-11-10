//! Subscriptions: the records a caller asked for, as a snapshot, then each
//! record that changed or was removed, until the client hangs up. A
//! subscription holds an operation guard only while it writes, so a daemon
//! restart closes it between writes and the client subscribes again.
mod agents;
mod snapshot;

use crate::client::scope_tokens::ScopeToken;
use std::{sync::Arc, time::Duration};

use anyhow::Result;
use tokio::net::{
    UnixStream,
    unix::{OwnedReadHalf, OwnedWriteHalf},
};

use super::{
    answer::{self, OperationGuard},
    log,
    scope::Caller,
    workspace::Manager,
};
use crate::{
    client::{log::LogTopic, subscriptions::records::Topic},
    protocol::{Body, ErrorCode, Response},
};
use snapshot::{Records, Topics};

/// Lets a burst of state changes settle into one comparison.
const SETTLE: Duration = Duration::from_millis(200);

/// Who subscribes: the caller, and the scope token it sent, which must
/// stay valid for as long as the subscription runs.
pub(super) struct Subscriber {
    pub manager: Arc<Manager>,
    pub caller: Option<Caller>,
    pub scope: Option<ScopeToken>,
}

impl Subscriber {
    /// Send the snapshot of `topics` under `startup`, then each change.
    pub(super) async fn run(
        self,
        stream: UnixStream,
        id: u64,
        topics: Vec<Topic>,
        log: Option<LogTopic>,
        startup: OperationGuard,
    ) -> Result<()> {
        let manager = &self.manager;
        // Watch before reading, so a change in between still wakes it.
        let mut stored = manager.store.watch_changes();
        let mut agents = manager.signals.watch_agents();
        let (mut reader, mut writer) = stream.into_split();
        let taken = async {
            let log = match log {
                Some(log) => Some(manager.pin_log(log).await?),
                None => None,
            };
            let topics = Topics::new(&topics, log, self.caller.as_ref());
            let records = snapshot::take(manager, self.caller.as_ref(), &topics).await?;
            anyhow::Ok((topics, records))
        };
        let (topics, mut sent) = match taken.await {
            Ok(taken) => taken,
            Err(error) => {
                let body = answer::failure(&error);
                return answer::write_frames(&mut writer, Response::new(id, body)).await;
            }
        };
        let records = sent.values().map(|(record, _)| record.clone()).collect();
        answer::write_frames(&mut writer, Response::new(id, Body::Records(records))).await?;
        drop(startup);
        loop {
            tokio::select! {
                result = stored.changed() => result?,
                result = agents.changed() => result?,
                () = hangup(&mut reader) => return Ok(()),
            }
            tokio::time::sleep(SETTLE).await;
            stored.borrow_and_update();
            agents.borrow_and_update();
            if !self.still_scoped().await? {
                let body =
                    Body::error(ErrorCode::ScopeDenied, "expired or unknown workspace scope");
                return self.refuse(&mut writer, id, body).await;
            }
            let current = match snapshot::take(manager, self.caller.as_ref(), &topics).await {
                Ok(records) => records,
                // The next change compares again.
                Err(error) => {
                    log!("subscription: {error:#}");
                    continue;
                }
            };
            // A restarting daemon closes the subscription instead, and the
            // client subscribes again on its replacement.
            let Some(_operation) = manager.gates.try_operation() else {
                return Ok(());
            };
            for body in changes(&sent, &current) {
                answer::write_frames(&mut writer, Response::new(id, body)).await?;
            }
            sent = current;
        }
    }

    /// End the subscription with `body`, unless a daemon update quiesces
    /// operations, which closes the connection unanswered.
    async fn refuse(&self, writer: &mut OwnedWriteHalf, id: u64, body: Body) -> Result<()> {
        let Some(_operation) = self.manager.gates.try_operation() else {
            return Ok(());
        };
        answer::write_frames(writer, Response::new(id, body)).await
    }

    /// Whether the scope token still names a caller.
    async fn still_scoped(&self) -> Result<bool> {
        match &self.scope {
            Some(token) => Ok(self.manager.caller(token).await?.is_some()),
            None => Ok(true),
        }
    }
}

/// Wait until the client hangs up. It sends nothing on a subscription, so
/// anything readable means it closed the connection.
async fn hangup(reader: &mut OwnedReadHalf) {
    let mut closed = [0u8; 1];
    let _ = tokio::io::AsyncReadExt::read(reader, &mut closed).await;
}

/// What changed from `before` to `after`: each new or changed record, then
/// each removed one.
fn changes(before: &Records, after: &Records) -> Vec<Body> {
    let changed = after
        .iter()
        .filter(|(key, (_, value))| before.get(*key).is_none_or(|(_, sent)| sent != value))
        .map(|(_, (record, _))| Body::RecordChanged(record.clone()));
    let removed = before
        .keys()
        .filter(|key| !after.contains_key(*key))
        .map(|key| Body::RecordRemoved(key.clone()));
    changed.chain(removed).collect()
}

#[cfg(test)]
mod tests;
