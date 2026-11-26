//! The hub's forwarding of signed HTTP requests to the local servers
//! `[machine.hub] forward` names.
use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, anyhow, ensure};
use http_body_util::{BodyExt as _, Full, combinators::BoxBody};
use hyper::{
    Request, Response, StatusCode,
    body::{Bytes, Incoming},
    header::HeaderMap,
};
use hyper_util::rt::TokioIo;
use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::{Nonce, Signature, app_servers};
use tokio::sync::OwnedSemaphorePermit;

use super::{identity::verify, members};
use crate::{Hub, log};

/// The body of a response the listener sends.
pub(super) type Body = BoxBody<Bytes, hyper::Error>;

impl Hub {
    /// The response to `request` from the server at `address`, once its
    /// member proves it signed it; the listener's `permit` for its
    /// connection stays held while an upgrade of it goes on.
    pub(super) async fn forward_to_server(
        &self,
        mut request: Request<Incoming>,
        address: SocketAddr,
        permit: Arc<OwnedSemaphorePermit>,
    ) -> Response<Body> {
        if let Err(error) = self.prove_member(&request).await {
            log!(
                "hub: refused a request to forward: {}",
                super::shown(&format!("{error:#}"))
            );
            return text(
                StatusCode::UNAUTHORIZED,
                "a paired member signs every request here",
            );
        }
        let headers = request.headers_mut();
        for name in [
            app_servers::TIME,
            app_servers::NONCE,
            app_servers::SIGNATURE,
        ] {
            headers.remove(name);
        }
        match forward(request, address, permit).await {
            Ok(response) => response,
            Err(error) => {
                log!("hub: could not forward a request to {address}: {error:#}");
                text(
                    StatusCode::BAD_GATEWAY,
                    "the server behind the hub did not answer",
                )
            }
        }
    }

    /// Check that a paired member the hub has not revoked signed
    /// `request` lately, and with a nonce no request took before.
    async fn prove_member(&self, request: &Request<Incoming>) -> Result<()> {
        let claim = Claim::of(request.headers())?;
        let now = crate::time::UnixTime::now().seconds();
        ensure!(
            now.unsigned_abs().abs_diff(claim.time) <= app_servers::SKEW.as_secs(),
            "it was signed at {}, too far from now",
            claim.time
        );
        let own = self.hub_identity().await?.public.clone();
        let member = claim.member.clone();
        let recorded = (self.store)
            .read(move |tx| members::find(tx, &member, &own))
            .await?;
        let key = match recorded {
            Some(recorded) if !recorded.revoked => recorded.key,
            _ => return Err(anyhow!("no paired member {}", claim.member)),
        };
        let target = request
            .uri()
            .path_and_query()
            .map_or("/", |target| target.as_str());
        let signed = app_servers::signed(
            &claim.member,
            claim.time,
            &claim.nonce,
            request.method().as_str(),
            target,
        );
        ensure!(
            verify(&key, &signed, &claim.signature),
            "its signature does not check"
        );
        let (nonce, expires_at) = (claim.nonce, claim.time + app_servers::SKEW.as_secs());
        let taken = (self.store).write(move |tx| take_nonce(tx, &nonce, expires_at));
        ensure!(taken.await?, "its nonce was used before");
        Ok(())
    }
}

/// Take `nonce`, unless a request took it before, keeping it until
/// `expires_at`, when no request signed with it is taken any longer.
fn take_nonce(tx: &rusqlite::Transaction<'_>, nonce: &Nonce, expires_at: u64) -> Result<bool> {
    let now = crate::time::UnixTime::now();
    tx.execute(
        "DELETE FROM hub_forward_nonces WHERE expires_at < ?1",
        [now],
    )?;
    let taken = tx.execute(
        "INSERT OR IGNORE INTO hub_forward_nonces(nonce, expires_at) VALUES (?1, ?2)",
        rusqlite::params![nonce, expires_at as i64],
    )?;
    Ok(taken == 1)
}

/// What a request's headers claim of who signed it.
struct Claim {
    member: MemberId,
    time: u64,
    nonce: Nonce,
    signature: Signature,
}

impl Claim {
    fn of(headers: &HeaderMap) -> Result<Self> {
        // One value each, so the member the server reads is the one the
        // signature proves, whichever of several it would take.
        let header = |name: &str| -> Result<&str> {
            let mut values = headers.get_all(name).iter();
            let value = values.next().with_context(|| format!("no {name} header"))?;
            ensure!(values.next().is_none(), "more than one {name} header");
            value
                .to_str()
                .with_context(|| format!("an unreadable {name} header"))
        };
        Ok(Self {
            member: MemberId::from(header(app_servers::MEMBER)?.to_owned()),
            time: header(app_servers::TIME)?
                .parse()
                .context("an unreadable time")?,
            nonce: header(app_servers::NONCE)?.parse()?,
            signature: header(app_servers::SIGNATURE)?.parse()?,
        })
    }
}

/// How long the server at an address has to accept a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The server at `address`'s response to `request`, on a connection of
/// its own, joining the two connections, holding `permit`, once both sides
/// upgrade it.
async fn forward(
    mut request: Request<Incoming>,
    address: SocketAddr,
    permit: Arc<OwnedSemaphorePermit>,
) -> Result<Response<Body>> {
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(address))
        .await
        .context("it did not accept the connection in time")??;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        if let Err(error) = connection.with_upgrades().await {
            log!("hub: a forwarded connection ended: {error}");
        }
    });
    let upgrading = (request.headers().contains_key(hyper::header::UPGRADE))
        .then(|| hyper::upgrade::on(&mut request));
    let mut response = sender.send_request(request).await?;
    if let (Some(upgrading), StatusCode::SWITCHING_PROTOCOLS) = (upgrading, response.status()) {
        let upgraded = hyper::upgrade::on(&mut response);
        tokio::spawn(join_upgrade(upgrading, upgraded, permit));
    }
    Ok(response.map(|body| body.boxed()))
}

/// Carry bytes both ways between the client's and the server's upgraded
/// connections until either ends, holding the client connection's
/// `permit` until then.
async fn join_upgrade(
    client: hyper::upgrade::OnUpgrade,
    server: hyper::upgrade::OnUpgrade,
    permit: Arc<OwnedSemaphorePermit>,
) {
    let joined = async {
        let (client, server) = tokio::try_join!(client, server)?;
        let (mut client, mut server) = (TokioIo::new(client), TokioIo::new(server));
        tokio::io::copy_bidirectional(&mut client, &mut server).await?;
        anyhow::Ok(())
    };
    if let Err(error) = joined.await {
        log!("hub: a forwarded upgrade ended: {error:#}");
    }
    drop(permit);
}

/// A response with `status` and `message` as its text.
pub(super) fn text(status: StatusCode, message: &str) -> Response<Body> {
    let body = Full::new(Bytes::from(format!("{message}\n")));
    let mut response = Response::new(body.map_err(|never| match never {}).boxed());
    *response.status_mut() = status;
    response
}

#[cfg(test)]
mod tests;
