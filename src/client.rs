use std::{io, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use tokio::{
    net::UnixStream,
    time::{Instant, sleep, timeout},
};

use crate::{
    paths::Paths,
    protocol::{self, Body, Method, Request, Response, Status},
};

pub async fn call(paths: &Paths, method: Method) -> Result<Body> {
    let seconds = if matches!(method, Method::Status | Method::Shutdown) {
        3
    } else {
        3600
    };
    timeout(Duration::from_secs(seconds), async {
        let mut stream = UnixStream::connect(&paths.socket).await.with_context(|| {
            format!(
                "connect to {}; run `shoal setup` or `shoal daemon start`",
                paths.socket.display()
            )
        })?;
        protocol::write(
            &mut stream,
            &Request {
                protocol: protocol::VERSION,
                id: 1,
                method,
            },
        )
        .await?;
        let reply: Response = protocol::read(&mut stream).await?;
        ensure!(
            reply.protocol == protocol::VERSION,
            "daemon protocol mismatch; restart the daemon with the installed version"
        );
        ensure!(reply.id == 1, "unexpected daemon response ID");
        match reply.body {
            Body::Error { code, message } => bail!("{code}: {message}"),
            result => Ok(result),
        }
    })
    .await
    .context("daemon request timed out")?
}

pub async fn status(paths: &Paths) -> Result<Option<Status>> {
    match call(paths, Method::Status).await {
        Ok(Body::Status(status)) => Ok(Some(status)),
        Ok(_) => bail!("unexpected status response"),
        Err(error)
            if error
                .chain()
                .filter_map(|e| e.downcast_ref::<io::Error>())
                .any(|e| {
                    matches!(
                        e.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    )
                }) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub async fn wait(paths: &Paths, running: bool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let last_error = match status(paths).await {
            Ok(status) if status.is_some() == running => return Ok(()),
            Ok(_) => None,
            Err(error) => Some(error),
        };
        ensure!(
            Instant::now() < deadline,
            "daemon did not {} within 10 seconds{}",
            if running { "start" } else { "stop" },
            last_error.map(|e| format!(": {e:#}")).unwrap_or_default()
        );
        sleep(Duration::from_millis(100)).await;
    }
}
