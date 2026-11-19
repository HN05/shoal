//! The forge each host serves that this process knows: the types the
//! caller pins and the answers hosts gave, which parsing a link of the host
//! reads to pick the same forge.
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex, PoisonError, RwLock},
};

use crate::backends::ForgeKind;

/// The answers this process resolved.
static LEARNED: LazyLock<RwLock<HashMap<String, ForgeKind>>> = LazyLock::new(Default::default);

/// The types the caller pins, which outrank answers so that one resolved
/// before the pin changed never replaces it.
static PINNED: LazyLock<RwLock<HashMap<String, ForgeKind>>> = LazyLock::new(Default::default);

/// The hosts the caller pins, read the first time a host's forge is needed.
type Pins = Box<dyn FnOnce() -> Vec<(String, ForgeKind)> + Send>;

static PINS: Mutex<Option<Pins>> = Mutex::new(None);

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
pub fn learned(host: &str) -> Option<ForgeKind> {
    pinned(host).or_else(|| {
        LEARNED
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(host)
            .copied()
    })
}

/// What this process learned `host` serves, else the well-known hosts by
/// name, else `linked`, the forge whose routes a link of the host uses;
/// failing all three a host whose name has a `gitlab` label counts as
/// GitLab and any other as Forgejo or Gitea.
pub fn detect(host: &str, linked: Option<ForgeKind>) -> ForgeKind {
    learned(host)
        .or_else(|| ForgeKind::named(host))
        .or(linked)
        .unwrap_or_else(|| {
            if host.split(['.', ':']).any(|label| label == "gitlab") {
                ForgeKind::GitLab
            } else {
                ForgeKind::Forgejo
            }
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
