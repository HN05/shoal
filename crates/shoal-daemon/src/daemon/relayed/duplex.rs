//! Relaying a request another machine answers on a duplex stream, as a
//! demo stream's: the first frame that machine's daemon writes is the
//! answer, which goes to the caller under its own request's ID, and after
//! it the bytes both ways are the connection's, relayed unread, until the
//! other machine ends them, the caller hangs up, or the request's scope
//! token stops acting for a caller.
use anyhow::{Result, bail};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
};

use super::{Relayed, body_frame};
use crate::{
    daemon::{
        answer,
        hub::{Incoming, Outgoing},
    },
    refusal::{Reason, Refusal},
};

/// The most bytes of the caller's read at once.
const READ_BYTES: usize = 64 << 10;

impl Relayed<'_> {
    /// Relay `stream`, the caller's connection, to and from the other
    /// machine's daemon: its answer comes on `incoming`, then the
    /// connection's bytes, and the caller's go on `outgoing`.
    pub(in crate::daemon) async fn relay_duplex(
        &self,
        stream: UnixStream,
        mut incoming: Incoming,
        outgoing: Outgoing,
    ) -> Result<()> {
        let (reader, mut writer) = stream.into_split();
        let Some(after) = self.answer(&mut incoming, &mut writer).await? else {
            return Ok(());
        };
        // What comes down decides when the connection ends; the caller's
        // bytes end it only by failing.
        tokio::select! {
            // A connection either side breaks off ends as one that closes.
            _ = pass_down(after, incoming, writer) => {}
            Err(_) = pass_up(reader, outgoing) => {}
            () = self.scope_revoked() => {}
        }
        Ok(())
    }

    /// Write the answer `incoming` brings first to `writer`, under the
    /// caller's request's ID and a daemon operation, and return the bytes
    /// that came after it; none once it broke off, which the caller hears
    /// instead, or while a restart holds operations off, which closes the
    /// connection unanswered for the caller to ask again.
    async fn answer(
        &self,
        incoming: &mut Incoming,
        writer: &mut OwnedWriteHalf,
    ) -> Result<Option<Vec<u8>>> {
        match answer_frame(incoming).await {
            Ok((frame, after)) => Ok(self.write(writer, vec![frame]).await?.then_some(after)),
            Err(error) => {
                let error = error.context("the other machine's answer broke off");
                let error = Refusal::wrap(Reason::MachineUnreachable, error);
                let failure = body_frame(self.request_id, answer::failure(&error))?;
                self.write(writer, vec![failure]).await?;
                Ok(None)
            }
        }
    }

    /// Resolves once the request's scope token no longer acts for a
    /// caller; never for the user's.
    async fn scope_revoked(&self) {
        let mut checks = tokio::time::interval(super::SCOPE_CHECK);
        loop {
            checks.tick().await;
            if let Ok(false) = self
                .manager
                .coordination
                .still_scoped(self.manager, self.scope)
                .await
            {
                return;
            }
        }
    }
}

/// The first frame `incoming` brings, as JSON, with the bytes that came
/// after it.
async fn answer_frame(incoming: &mut Incoming) -> Result<(serde_json::Value, Vec<u8>)> {
    let mut read = Vec::new();
    loop {
        if let Some(end) = read.iter().position(|byte| *byte == b'\n') {
            let after = read.split_off(end + 1);
            return Ok((serde_json::from_slice(&read)?, after));
        }
        if read.len() > shoal_protocol::protocol::MAX_FRAME {
            bail!("the answer's first frame is longer than a frame may be");
        }
        match incoming.next().await? {
            Some(bytes) => read.extend(bytes),
            None => bail!("it ended before its first frame"),
        }
    }
}

/// Write `after`, then what `incoming` brings, to the caller's `writer`,
/// until it ends.
async fn pass_down(
    after: Vec<u8>,
    mut incoming: Incoming,
    mut writer: OwnedWriteHalf,
) -> Result<()> {
    writer.write_all(&after).await?;
    while let Some(bytes) = incoming.next().await? {
        writer.write_all(&bytes).await?;
    }
    Ok(())
}

/// Write what the caller sends to `outgoing`, ending it once the caller
/// ends its side; then never resolves. Once the other machine stops
/// reading, as when its side of the connection ended, it passes nothing
/// more up and leaves what still comes down to end the connection.
async fn pass_up(mut reader: OwnedReadHalf, mut outgoing: Outgoing) -> Result<()> {
    let mut bytes = vec![0; READ_BYTES];
    loop {
        let read = tokio::select! {
            read = reader.read(&mut bytes) => read?,
            () = outgoing.closed() => return std::future::pending().await,
        };
        if read == 0 {
            break;
        }
        if outgoing.write(&bytes[..read]).await.is_err() {
            return std::future::pending().await;
        }
    }
    outgoing.end(None).await;
    std::future::pending().await
}
