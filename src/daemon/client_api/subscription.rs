//! A subscription: its snapshot in pages, then each record that changed or
//! was removed, until the client hangs up. It holds an operation guard only
//! while it writes, so a daemon restart closes it between writes and the
//! client subscribes again.
use std::time::Duration;

use anyhow::Result;
use shoal_client::{
    Subscribe,
    wire::{self, Event, Reply, Response},
};
use tokio::{io::AsyncReadExt as _, net::unix::OwnedWriteHalf};

use super::{
    Reader, Session, errors,
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
    let caller = session.caller.as_ref();
    let topics = Topics::new(&subscribe.topics, caller);
    // Watch before reading, so a change in between still wakes it.
    let mut stored = manager.store.watch_changes();
    let mut agents = manager.signals.watch_agents();
    let mut sent = match snapshot::take(&manager, caller, &topics).await {
        Ok(records) => records,
        Err(error) => {
            let reply = Reply::Error(errors::failed(&error));
            return Ok(wire::write(&mut writer, &Response { id, reply }).await?);
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
        let current = match snapshot::take(&manager, caller, &topics).await {
            Ok(records) => records,
            // The next change compares again.
            Err(error) => {
                log!("client API subscription: {error:#}");
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

/// Wait until the client hangs up. It sends nothing after subscribing, so
/// anything readable means it closed the connection.
async fn hangup(reader: &mut Reader) {
    let mut closed = [0u8; 1];
    let _ = reader.read(&mut closed).await;
}

#[cfg(test)]
mod tests;
