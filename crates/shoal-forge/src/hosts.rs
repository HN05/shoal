//! Which forge a host serves. Hosts outside the well-known names are asked
//! through each forge's version or metadata endpoint; the caller decides
//! what it already knows, such as a configured type or an earlier answer,
//! and this process remembers each answer so that parsing a link of the
//! host later picks the same forge.
use std::{
    collections::HashMap,
    future::Future,
    sync::{LazyLock, PoisonError, RwLock},
};

use anyhow::{Result, anyhow};

use crate::{
    ForgeRepo,
    account::Account,
    backends::{Backend, ForgeKind},
};

static LEARNED: LazyLock<RwLock<HashMap<String, ForgeKind>>> = LazyLock::new(Default::default);

/// Where the forge of a host comes from: settings, a recorded answer, or
/// [`probe`].
pub trait HostTypes: Sync {
    /// The forge `host` serves, reached over `scheme` (`http` or `https`).
    fn forge_type(
        &self,
        host: &str,
        scheme: &'static str,
    ) -> impl Future<Output = Result<ForgeKind>> + Send;
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
            Ok(_) => format!("{kind}: not its API"),
            Err(error) => format!("{kind}: {}", one_line(&format!("{error:#}"))),
        })
        .collect();
    Err(anyhow!("{}", reasons.join("; ")).context(format!(
        "{host} answers as none of GitHub, GitLab, Forgejo, Gitea or Bitbucket"
    )))
}

/// `text` with its lines joined by spaces.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Remember for this process that `host` serves `kind`.
pub fn learn(host: &str, kind: ForgeKind) {
    LEARNED
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(host.to_ascii_lowercase(), kind);
}

/// What this process learned `host` serves.
pub(crate) fn learned(host: &str) -> Option<ForgeKind> {
    LEARNED
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .get(host)
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learned_hosts_override_names_and_guesses() {
        assert_eq!(
            ForgeKind::detect("ghe.learned.example", None),
            ForgeKind::Forgejo
        );
        learn("GHE.learned.example", ForgeKind::GitHub);
        assert_eq!(
            ForgeKind::detect("ghe.learned.example", None),
            ForgeKind::GitHub
        );
        let repo = ForgeRepo::parse("git@ghe.learned.example:team/repo.git").unwrap();
        assert_eq!(
            repo.pull("3").unwrap().1,
            "https://ghe.learned.example/team/repo/pull/3"
        );
    }
}
