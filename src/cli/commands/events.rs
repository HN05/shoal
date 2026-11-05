//! `shoal internal events`: the integration stream that never consumes user notifications.
use crate::{
    cli::{
        context::Context,
        installed::{Executable, arguments_with},
    },
    client::events::{EventItem, Events},
};
use anyhow::Result;

pub(super) async fn run(ctx: &Context, follow: bool, since: Option<i64>) -> Result<i32> {
    let executable = Executable::current();
    let mut events = Events::open(&ctx.paths, since, follow).await?;
    loop {
        let next = events.next(&ctx.paths).await.map_err(|error| {
            let cursor = events.cursor();
            executable.resume_or(error, || arguments_with("--since", cursor))
        })?;
        let Some(item) = next else {
            return Ok(0);
        };
        ctx.show(&item, |item| match item {
            EventItem::Event(event) => {
                outln!("{} {} {}", event.id, event.details.kind, event.details.name)
            }
            EventItem::Chat(event) => outln!("{} {} {}", event.id, event.kind, event.chat.id),
            EventItem::File(event) => {
                outln!(
                    "{} {} {}",
                    event.id,
                    event.details.kind,
                    event.details.file.name
                )
            }
            EventItem::Gap {
                since,
                oldest_id,
                latest_id,
            } => outln!(
                "event gap after {since}; retained events start at {oldest_id} (latest {latest_id})"
            ),
        })?;
    }
}
