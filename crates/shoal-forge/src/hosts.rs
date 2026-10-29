//! Which forge a host serves. Hosts outside the well-known names are asked
//! through each forge's version or metadata endpoint; the caller decides
//! what it already knows, such as a configured type or an earlier answer,
//! and this process remembers each answer so that parsing a link of the
//! host later picks the same forge.
use std::{
    collections::HashMap,
    future::Future,
    sync::{LazyLock, Mutex, PoisonError, RwLock},
};

use anyhow::{Result, anyhow};

use crate::{
    ForgeRepo,
    account::Account,
    backends::{Backend, ForgeKind},
};

/// The answers this process resolved.
static LEARNED: LazyLock<RwLock<HashMap<String, ForgeKind>>> = LazyLock::new(Default::default);

/// The types the caller pins, which outrank answers so that one resolved
/// before the pin changed never replaces it.
static PINNED: LazyLock<RwLock<HashMap<String, ForgeKind>>> = LazyLock::new(Default::default);

/// The hosts the caller pins, read the first time a host's forge is needed.
type Pins = Box<dyn FnOnce() -> Vec<(String, ForgeKind)> + Send>;

static PINS: Mutex<Option<Pins>> = Mutex::new(None);

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

/// Remember for this process that `host` serves `kind`.
pub fn learn(host: &str, kind: ForgeKind) {
    LEARNED
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(host.to_ascii_lowercase(), kind);
}

/// Make `pins` the types this process pins, replacing any it had and any
/// [`learn_pins`] has yet to load.
pub fn pin(pins: Vec<(String, ForgeKind)>) {
    let mut pending = PINS.lock().unwrap_or_else(PoisonError::into_inner);
    *pending = None;
    replace_pins(pins);
}

fn replace_pins(pins: Vec<(String, ForgeKind)>) {
    *PINNED.write().unwrap_or_else(PoisonError::into_inner) = pins
        .into_iter()
        .map(|(host, kind)| (host.to_ascii_lowercase(), kind))
        .collect();
}

/// [`pin`] what `pins` returns the first time this process needs a host's
/// forge, so a caller that never parses a forge link never reads them.
pub fn learn_pins(pins: impl FnOnce() -> Vec<(String, ForgeKind)> + Send + 'static) {
    *PINS.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(pins));
}

/// The type pinned for `host`, else what this process learned it serves,
/// once the caller's pins are loaded; a concurrent first reader waits for
/// them.
pub(crate) fn learned(host: &str) -> Option<ForgeKind> {
    pinned(host).or_else(|| {
        LEARNED
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(host)
            .copied()
    })
}

/// The type pinned for `host`, once the caller's pins are loaded.
fn pinned(host: &str) -> Option<ForgeKind> {
    let mut pins = PINS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(load) = pins.take() {
        replace_pins(load());
    }
    drop(pins);
    PINNED
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .get(host)
        .copied()
}

#[cfg(test)]
mod tests;
