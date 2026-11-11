//! What a start the daemon runs later, away from any terminal, needs from
//! the place it comes from.
use std::collections::BTreeMap;

use super::{Host, Places, here::current_origin, hosts};
use crate::{env, sessions::Origin};

/// What a start the daemon runs later for this process needs, without a
/// terminal: the place it comes from, the hosts' servers this process
/// chooses, and the output setting.
pub fn start_environment() -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([env::output_setting()]);
    if let Some(origin) = current_origin().and_then(|origin| serde_json::to_string(&origin).ok()) {
        env.insert(env::ORIGIN.to_owned(), origin);
    }
    env.extend(server_selection(|name| std::env::var(name).ok()));
    env
}

/// The variables of a start that comes from no terminal, such as one an
/// app queues through the protocol or the daemon queues for a stopped
/// agent: `origin`, where that agent ran, and every host's server unset, so
/// it opens there or, without one, in `terminal.default_host` whatever the
/// daemon's own environment names.
pub fn detached_start_environment(origin: Option<&Origin>) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([env::output_setting()]);
    let origin = origin
        .and_then(|origin| serde_json::to_string(origin).ok())
        .unwrap_or_else(|| NO_ORIGIN.to_owned());
    env.insert(env::ORIGIN.to_owned(), origin);
    env.extend(server_selection(|_| None));
    env
}

/// The recorded origin of a start that comes from no place.
const NO_ORIGIN: &str = "null";

/// The variables that choose a host's server, with the values `var` gives
/// them, so a start queued outside the host's places opens its place on
/// that server. An unset one is empty, which hosts read as unset, so the
/// daemon's own value does not stand in for it.
fn server_selection(
    var: impl Fn(&str) -> Option<String>,
) -> impl Iterator<Item = (String, String)> {
    hosts()
        .filter_map(Host::places)
        .flat_map(Places::server_variables)
        .map(move |name| ((*name).to_owned(), var(name).unwrap_or_default()))
}

#[cfg(test)]
mod tests;
