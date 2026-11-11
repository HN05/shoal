//! How an agent's API requests reach Shoal's proxy: whether an integration
//! can be pointed there, and the arguments and environment that point it.
use crate::agent::Agent;
use std::{collections::BTreeMap, ffi::OsString, path::Path};

use anyhow::{Context, Result, ensure};

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

/// The URL an agent with integration `kind` takes for `proxy`, running at
/// `url`: under the path of its API when the proxy forwards to an
/// `upstream`, a server root, and `url` itself when it forwards to the
/// agent's own base URL.
pub(crate) fn agent_url(kind: IntegrationKind, url: &str, proxy: &crate::proxy::Proxy) -> String {
    match proxy.upstream {
        Some(_) => format!("{url}{}", integration(kind).api_path()),
        None => url.to_owned(),
    }
}

/// Where `proxy` forwards the requests of an agent with integration `kind`
/// launched as `command` in `directory`, with its user files under `home`
/// and the environment `variable` reads, its own removals included: its
/// `upstream`, else where the agent's requests go without it.
pub(crate) fn upstream(
    kind: IntegrationKind,
    proxy: &crate::proxy::Proxy,
    command: &[OsString],
    directory: &Path,
    variable: &dyn Fn(&str) -> Option<OsString>,
    home: &Path,
) -> Result<String> {
    if let Some(upstream) = &proxy.upstream {
        return Ok(upstream.clone());
    }
    integration(kind)
        .default_upstream(command, directory, variable, home)
        .context("find where the agent's API requests go without its proxy")
}

/// Whether an agent with integration `kind` can go through Shoal's proxy.
fn routes_through_proxy(kind: IntegrationKind) -> bool {
    integration(kind).routes_through_proxy()
}

/// Where a proxied agent's launch goes: its proxy's `url`, the `directory`
/// it runs in, and a `private` directory only its user can read, for what
/// must stay off its command line, which any local user can read.
pub struct Routing<'a> {
    pub(crate) url: &'a str,
    pub(crate) directory: &'a Path,
    pub(crate) private: &'a Path,
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
pub(crate) fn with_proxy_args(settings: &Effective, name: &Agent, command: &mut Vec<OsString>) {
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
    name: &Agent,
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
