//! Following an operation: its events from the one asked for on, then its
//! result or error, until it ends or the client hangs up. It holds an
//! operation guard only while it writes, so a daemon restart closes it
//! between writes.
use anyhow::Result;
use shoal_client::{
    FollowOperation,
    wire::{self, Reply, Response},
};
use tokio::net::unix::OwnedWriteHalf;

use super::{Reader, Session, errors, hangup, progress};

pub(super) async fn run(
    session: Session,
    id: u64,
    follow: FollowOperation,
    mut reader: Reader,
    mut writer: OwnedWriteHalf,
) -> Result<()> {
    let manager = session.server.manager.clone();
    let entry = match session.current_caller().await {
        Ok(caller) => manager
            .operation(&follow.operation, caller.as_ref())
            .await
            .map_err(|error| errors::scope_refused(&error)),
        Err(error) => Err(error),
    };
    let entry = match entry {
        Ok(entry) => entry,
        Err(error) => {
            let reply = Reply::Error(error);
            super::write_guarded(&manager, &mut writer, &Response { id, reply }).await?;
            return Ok(());
        }
    };
    let mut changed = entry.watch();
    let mut next = follow.after;
    loop {
        changed.borrow_and_update();
        let (events, ended) = entry.since(next);
        let ended = match ended {
            Some(ended) => Some(progress::ended(&session, ended).await),
            None => None,
        };
        let Some(operation) = manager.gates.try_operation() else {
            return Ok(());
        };
        for event in events {
            next = event.index + 1;
            let reply = Reply::Progress(progress::event(event));
            wire::write(&mut writer, &Response { id, reply }).await?;
        }
        if let Some(reply) = ended {
            wire::write(&mut writer, &Response { id, reply }).await?;
            return Ok(());
        }
        drop(operation);
        tokio::select! {
            result = changed.changed() => result?,
            () = hangup(&mut reader) => return Ok(()),
        }
    }
}
