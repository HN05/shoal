//! `shoal events`: the integration stream that never consumes user notifications.
use crate::{
    cli::{client, context::Context},
    daemon::events::EventItem,
    protocol::{self, Body, Method, Response},
};
use anyhow::{Context as _, Result};

pub(super) async fn run(ctx: &Context, follow: bool, since: Option<i64>) -> Result<i32> {
    let (mut reader, body) =
        client::open_buffered(&ctx.paths, Method::WatchWorkspaceEvents { since, follow }).await?;
    <()>::try_from(body)?;
    loop {
        let response: Response = protocol::read_buffered(&mut reader)
            .await
            .context("daemon closed the event stream")?;
        let item = match response.body {
            Body::Ok if !follow => return Ok(0),
            Body::EventItem(item) => item,
            body => return Err(body.unexpected("EventItem")),
        };
        ctx.show(&item, |item| match item {
            EventItem::Event(event) => {
                println!("{} {} {}", event.id, event.details.kind, event.details.name)
            }
            EventItem::Gap {
                since,
                oldest_id,
                latest_id,
            } => println!(
                "event gap after {since}; retained events start at {oldest_id} (latest {latest_id})"
            ),
        })?;
    }
}
