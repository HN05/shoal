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
    let mut cursor = since;
    loop {
        let response: Response = match protocol::read_buffered(&mut reader).await {
            Ok(response) => response,
            Err(error) if follow && client::stream_closed(&error) => {
                let (next, body) = client::reconnect(&ctx.paths, || Method::WatchWorkspaceEvents {
                    since: cursor,
                    follow,
                })
                .await?;
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
        cursor = Some(match item {
            EventItem::Event(event) => event.id,
            EventItem::Gap { oldest_id, .. } => oldest_id - 1,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{paths::Paths, protocol::Request};
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn follower_reconnects_at_the_last_displayed_cursor() {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let paths = Paths::for_test(root.path());
        paths.prepare().unwrap();
        let listener = UnixListener::bind(&paths.socket).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request: Request = protocol::read(&mut stream).await.unwrap();
            assert!(matches!(
                request.method,
                Method::WatchWorkspaceEvents {
                    since: Some(9),
                    follow: true
                }
            ));
            protocol::write(&mut stream, &Response::new(request.id, Body::Ok))
                .await
                .unwrap();
            protocol::write(
                &mut stream,
                &Response::new(
                    request.id,
                    Body::EventItem(EventItem::Gap {
                        since: 9,
                        oldest_id: 4,
                        latest_id: 5,
                    }),
                ),
            )
            .await
            .unwrap();
            drop(stream);
            let (mut stream, _) = listener.accept().await.unwrap();
            let request: Request = protocol::read(&mut stream).await.unwrap();
            assert!(matches!(
                request.method,
                Method::WatchWorkspaceEvents {
                    since: Some(3),
                    follow: true
                }
            ));
            // Finish the infinite follower with an explicit terminal response.
            protocol::write(&mut stream, &Response::new(request.id, Body::Ok))
                .await
                .unwrap();
            protocol::write(
                &mut stream,
                &Response::new(
                    request.id,
                    Body::Busy {
                        message: "fixture end".into(),
                    },
                ),
            )
            .await
            .unwrap();
        });
        let ctx = Context::new(paths, true);
        let error =
            tokio::time::timeout(std::time::Duration::from_secs(60), run(&ctx, true, Some(9)))
                .await
                .unwrap()
                .unwrap_err();
        assert!(error.to_string().contains("received Busy"));
        server.await.unwrap();
    }
}
