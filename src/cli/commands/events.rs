//! `shoal internal events`: the integration stream that never consumes user notifications.
use crate::{
    cli::{
        client,
        context::Context,
        installed::{Executable, arguments_with},
    },
    daemon::events::EventItem,
    protocol::{self, Body, Response, WorkspaceMethod},
};
use anyhow::{Context as _, Result};

pub(super) async fn run(ctx: &Context, follow: bool, since: Option<i64>) -> Result<i32> {
    let executable = Executable::current();
    let (mut reader, body) = client::open_buffered(
        &ctx.paths,
        WorkspaceMethod::WatchWorkspaceEvents { since, follow },
    )
    .await?;
    <()>::try_from(body)?;
    let mut cursor = since;
    loop {
        let response: Response = match protocol::read_buffered(&mut reader).await {
            Ok(response) => response,
            Err(error) if follow && client::stream_closed(&error) => {
                let (next, body) =
                    client::reconnect(&ctx.paths, || WorkspaceMethod::WatchWorkspaceEvents {
                        since: cursor,
                        follow,
                    })
                    .await
                    .map_err(|error| {
                        executable.resume_or(error, || arguments_with("--since", cursor))
                    })?;
                <()>::try_from(body)?;
                reader = next;
                continue;
            }
            Err(error) => return Err(error).context("daemon closed the event stream"),
        };
        let item = match response.body {
            Body::Ok if !follow => return Ok(0),
            Body::EventItem(item) => item,
            body => return Err(body.unexpected("EventItem")),
        };
        ctx.show(&item, |item| match item {
            EventItem::Event(event) => {
                outln!("{} {} {}", event.id, event.details.kind, event.details.name)
            }
            EventItem::Chat(event) => outln!("{} {} {}", event.id, event.kind, event.chat.id),
            EventItem::Gap {
                since,
                oldest_id,
                latest_id,
            } => outln!(
                "event gap after {since}; retained events start at {oldest_id} (latest {latest_id})"
            ),
        })?;
        cursor = Some(item.cursor());
    }
}

#[cfg(test)]
mod tests;
