//! A subscription: its snapshot in pages, then each record that changed or
//! was removed, until the client hangs up. It holds an operation guard only
//! while it writes, so a daemon restart closes it between writes and the
//! client subscribes again.
use std::time::Duration;

use anyhow::Result;
use shoal_client::{
    ApiError, ErrorCode, Subscribe,
    wire::{self, Event, Reply, Response},
};
use tokio::net::unix::OwnedWriteHalf;

use super::{
    Reader, Session, errors, hangup,
    snapshot::{self, Records, Topics},
};
use crate::daemon::log;

/// Lets a burst of state changes settle into one comparison.
const SETTLE: Duration = Duration::from_millis(200);

pub(super) async fn run(
    session: Session,
    id: u64,
    subscribe: Subscribe,
    mut reader: Reader,
    mut writer: OwnedWriteHalf,
) -> Result<()> {
    let manager = session.server.manager.clone();
    let topics = Topics::new(&subscribe.topics, session.caller.as_ref());
    // Watch before reading, so a change in between still wakes it.
    let mut stored = manager.store.watch_changes();
    let mut agents = manager.signals.watch_agents();
    let mut sent = match records(&session, &topics).await {
        Ok(records) => records,
        Err(error) => {
            let reply = Reply::Error(error);
            super::write_guarded(&manager, &mut writer, &Response { id, reply }).await?;
            return Ok(());
        }
    };
    {
        let Some(_operation) = manager.gates.try_operation() else {
            return Ok(());
        };
        send_snapshot(&mut writer, id, &sent).await?;
    }
    loop {
        tokio::select! {
            result = stored.changed() => result?,
            result = agents.changed() => result?,
            () = hangup(&mut reader) => return Ok(()),
        }
        tokio::time::sleep(SETTLE).await;
        stored.borrow_and_update();
        agents.borrow_and_update();
        let current = match records(&session, &topics).await {
            Ok(records) => records,
            Err(error) if error.code == ErrorCode::ScopeDenied => {
                let reply = Reply::Error(error);
                super::write_guarded(&manager, &mut writer, &Response { id, reply }).await?;
                return Ok(());
            }
            // The next change compares again.
            Err(error) => {
                log!("client API subscription: {error}");
                continue;
            }
        };
        // A restarting daemon closes the subscription instead, and the
        // client takes a new snapshot from its replacement.
        let Some(_operation) = manager.gates.try_operation() else {
            return Ok(());
        };
        for event in changes(&sent, &current) {
            send(&mut writer, id, event).await?;
        }
        sent = current;
    }
}

/// The records of `topics` that the session's caller may read now, once its
/// scope token still names it.
async fn records(session: &Session, topics: &Topics) -> Result<Records, ApiError> {
    let caller = session.current_caller().await?;
    snapshot::take(&session.server.manager, caller.as_ref(), topics)
        .await
        .map_err(|error| errors::failed(&error))
}

/// `records` as snapshot events that each stay within a page.
async fn send_snapshot(writer: &mut OwnedWriteHalf, id: u64, records: &Records) -> Result<()> {
    let mut pages = wire::pages(records.values().cloned().collect())?
        .into_iter()
        .peekable();
    while let Some(records) = pages.next() {
        let complete = pages.peek().is_none();
        send(writer, id, Event::Snapshot { records, complete }).await?;
    }
    Ok(())
}

/// What changed from `before` to `after`: each new or changed record, then
/// each removed one.
fn changes(before: &Records, after: &Records) -> Vec<Event> {
    let changed = after
        .iter()
        .filter(|(key, record)| before.get(*key) != Some(*record))
        .map(|(_, record)| Event::Changed {
            record: record.clone(),
        });
    let removed = before
        .keys()
        .filter(|key| !after.contains_key(*key))
        .map(|(kind, id)| Event::Removed {
            key: snapshot::key(kind, id),
        });
    changed.chain(removed).collect()
}

async fn send(writer: &mut OwnedWriteHalf, id: u64, event: Event) -> Result<()> {
    let reply = Reply::Event(event);
    Ok(wire::write(writer, &Response { id, reply }).await?)
}

#[cfg(test)]
mod tests;
