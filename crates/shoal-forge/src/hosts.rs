//! Which forge a host serves. Hosts outside the well-known names are asked
//! through each forge's version or metadata endpoint; the caller decides
//! what it already knows, such as a configured type or an earlier answer,
//! and this process remembers each answer so that parsing a link of the
//! host later picks the same forge.
use std::future::Future;

use anyhow::{Result, anyhow};

use crate::transport::account::Account;
use shoal_forge_types::{
    ForgeRepo,
    backends::{ForgeKind, Routes},
};

/// Where the forge of a host comes from: settings, a recorded answer, or
/// [`probe`].
pub trait HostTypes: Sync {
    /// The forge `host` serves, reached over `scheme` (`http` or `https`),
    /// and what decided it.
    fn forge_type(
        &self,
        host: &str,
        scheme: &'static str,
    ) -> impl Future<Output = Result<HostType>> + Send;
}

/// A host's forge and what decided it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostType {
    /// A pin or a well-known name, for which the host is never asked.
    Settled(ForgeKind),
    /// The host's own answer, now or when it was last asked.
    Answered(ForgeKind),
}

impl HostType {
    pub fn kind(self) -> ForgeKind {
        match self {
            Self::Settled(kind) | Self::Answered(kind) => kind,
        }
    }
}

/// How a forge tells a host that serves it.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Hosting: Routes {
    /// Whether `host`, a repository without a path, answers this forge's
    /// version or metadata endpoint as `account`; an error when it could
    /// not be asked.
    async fn serves(&self, host: &ForgeRepo, account: &Account) -> Result<bool>;
}

/// Ask `host` which forge's API it serves, using `account`'s login where
/// the forge needs one. Fails when it answers as none, with why each forge
/// did not as the error's cause.
pub async fn probe(host: &str, scheme: &'static str, account: &Account) -> Result<ForgeKind> {
    let answers = futures_util::future::join_all(ForgeKind::ALL.iter().map(|&kind| async move {
        let target = ForgeRepo::host(host, scheme, kind);
        (kind, kind.serves(&target, account).await)
    }))
    .await;
    if let Some((kind, _)) = answers
        .iter()
        .find(|(_, answer)| matches!(answer, Ok(true)))
    {
        return Ok(*kind);
    }
    let reasons: Vec<String> = answers
        .iter()
        .map(|(kind, answer)| match answer {
            Ok(_) => format!("{}: not its API", kind.name()),
            Err(error) => format!("{}: {}", kind.name(), one_line(&format!("{error:#}"))),
        })
        .collect();
    Err(anyhow!("{}", reasons.join("; "))
        .context(format!("{host} answers as none of the forges Shoal knows")))
}

/// `text` with its lines joined by spaces.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests;
