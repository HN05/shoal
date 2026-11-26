//! Shoal's retrying proxy between agents and their API upstream, configured
//! for each agent launch from the `[machine.proxies.<name>]` table its agent
//! names.
pub use crate::config::proxies::Proxy;

/// The header an agent names its proxy's key in, which the proxy checks.
pub const KEY_HEADER: &str = "x-shoal-proxy-key";

/// The variable of a proxied agent's environment that holds its proxy's
/// key, for an agent that names a variable rather than a value: the
/// environment is the agent's alone, while any local user can read its
/// command line.
pub(crate) const KEY_VARIABLE: &str = "SHOAL_PROXY_KEY";

#[cfg(test)]
mod tests;
