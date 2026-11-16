//! The journal of space, file and Shoal events, as `shoal internal events`
//! replays it.
use shoal_protocol::events::{EventCursor, EventItem};

use anyhow::{Context, Result};

use tokio::{io::BufReader, net::UnixStream};

use crate::connection::{Daemon, open_buffered, reconnect, stream_closed};
use shoal_protocol::protocol::{self, Body, Response, WorkspaceMethod};

/// The journal of space, file and Shoal events from a cursor: a replay
/// of the retained events, then with `follow` each new one as it happens,
/// across daemon restarts.
pub struct Events {
    reader: BufReader<UnixStream>,
    follow: bool,
    cursor: Option<EventCursor>,
}

impl Events {
    /// The events after `after`, or every retained one.
    pub async fn open(
        daemon: &impl Daemon,
        after: Option<EventCursor>,
        follow: bool,
    ) -> Result<Self> {
        let method = WorkspaceMethod::WatchSpaceEvents { after, follow };
        let (reader, body) = open_buffered(daemon, method).await?;
        <()>::try_from(body)?;
        Ok(Self {
            reader,
            follow,
            cursor: after,
        })
    }

    /// Where the stream resumes: after the last item it returned.
    pub fn cursor(&self) -> Option<EventCursor> {
        self.cursor
    }

    /// The next item; `None` once a replay without `follow` ends.
    pub async fn next(&mut self, daemon: &impl Daemon) -> Result<Option<EventItem>> {
        loop {
            let response: Response = match protocol::read_buffered(&mut self.reader).await {
                Ok(response) => response,
                Err(error) if self.follow && stream_closed(&error) => {
                    let (after, follow) = (self.cursor, self.follow);
                    let method = || WorkspaceMethod::WatchSpaceEvents { after, follow };
                    let (reader, body) = reconnect(daemon, method).await?;
                    <()>::try_from(body)?;
                    self.reader = reader;
                    continue;
                }
                Err(error) => return Err(error).context("daemon closed the event stream"),
            };
            let item = match response.body {
                Body::Ok if !self.follow => return Ok(None),
                Body::EventItem(item) => item,
                body => return Err(body.unexpected("EventItem")),
            };
            self.cursor = Some(item.cursor());
            return Ok(Some(item));
        }
    }
}

#[cfg(test)]
mod tests;
