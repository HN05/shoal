//! How an agent's API requests reach Shoal's proxy: whether an integration
//! can be pointed there, and the arguments and environment that point it.
use std::{collections::BTreeMap, ffi::OsString, path::Path};

use anyhow::{Result, ensure};

use super::{AgentEnvironment, args::insert_options, integration};
use crate::{ai::IntegrationKind, config::Effective};

/// Whether a failure stream from the proxy of an agent with integration
/// `kind` ends its turn.
pub(crate) fn proxy_ends_turns(kind: IntegrationKind) -> bool {
    integration(kind).proxy_ends_turns()
}

/// What the environment of an agent with integration `kind` gets so its
/// requests name its proxy's `key`, reading the environment it runs with
/// otherwise from `variable`.
pub(crate) fn proxy_environment(
    kind: IntegrationKind,
    key: &str,
    variable: &dyn Fn(&str) -> Option<OsString>,
) -> Vec<(&'static str, OsString)> {
    integration(kind).proxy_environment(key, variable)
}

/// The variables an agent with integration `kind` gets through its proxy
/// that `variable`, the environment it runs with otherwise, leaves unset.
pub(crate) fn proxy_defaults(
    kind: IntegrationKind,
    variable: &dyn Fn(&str) -> Option<OsString>,
) -> Vec<(&'static str, &'static str)> {
    integration(kind)
        .proxy_defaults()
        .iter()
        .copied()
        .filter(|(name, _)| variable(name).is_none())
        .collect()
}

/// Whether an agent with integration `kind` can go through Shoal's proxy.
fn routes_through_proxy(kind: IntegrationKind) -> bool {
    integration(kind).routes_through_proxy()
}

/// Where a proxied agent's launch goes: its proxy's `url`, the `directory`
/// it runs in, and a `private` directory only its user can read, for what
/// must stay off its command line, which any local user can read.
pub(crate) struct Routing<'a> {
    pub url: &'a str,
    pub directory: &'a Path,
    pub private: &'a Path,
}

/// `command` of an agent with integration `kind`, running with `configured`
/// added to this process's environment, sent to its proxy by `routing`.
pub(crate) fn route_through_proxy(
    kind: IntegrationKind,
    mut command: Vec<OsString>,
    routing: &Routing,
    home: &Path,
    configured: &BTreeMap<String, OsString>,
) -> Result<Vec<OsString>> {
    let environment = AgentEnvironment::new(home, configured);
    integration(kind).route_through_proxy(&mut command, routing, &environment)?;
    Ok(command)
}

/// `command`, a configured command of agent `name` that Shoal gives no
/// hooks, with the arguments that send its API requests to its proxy when it
/// has one it can use.
pub(crate) fn with_proxy_args(settings: &Effective, name: &str, command: &mut Vec<OsString>) {
    let kind = settings.integration(name);
    if settings.proxy(name).is_none() || !routes_through_proxy(kind) {
        return;
    }
    insert_options(command, integration(kind).proxy_args());
}

/// The proxy agent `name`'s API requests go through, if it names one; an
/// error when Shoal cannot point it there, for every launch, a resumed one
/// included.
pub(crate) fn launch_proxy<'a>(
    settings: &'a Effective,
    name: &str,
) -> Result<Option<&'a crate::proxy::Proxy>> {
    let Some(proxy) = settings.proxy(name) else {
        return Ok(None);
    };
    ensure!(
        integration(settings.integration(name)).routes_through_proxy(),
        "cannot start {name}: Shoal cannot send its API requests through a proxy; \
         remove proxy from its [machine.agents.{name}]"
    );
    Ok(Some(proxy))
}

#[cfg(test)]
mod tests;
