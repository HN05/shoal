//! The values of settings that a process of a space asks the daemon for,
//! as ones it cannot read itself.
use anyhow::{Context, Result, bail};

use crate::daemon::{manager::Manager, scope::Caller};
use shoal_protocol::{
    config::value::{Resolved, Value, ValueKey},
    space_names::SpaceSelector,
};

impl Manager {
    /// The value of `key` for the space `space`, as `caller` may read it:
    /// one the config writes out, which no file a launch writes holds, or
    /// one another machine prints, which only the daemon asks it for. The
    /// command run is always the daemon's own config's.
    pub(crate) async fn config_value(
        &self,
        space: &SpaceSelector,
        key: ValueKey,
        caller: Option<&Caller>,
    ) -> Result<Resolved> {
        let (setting, value) = match key {
            ValueKey::Secret { name, source } => {
                let leases = &self.leases;
                let secret = leases.readable_secret(self, space, name.clone(), source, caller);
                (format!("secrets.{name}.value"), secret.await?.value)
            }
            ValueKey::ProxyKey { key } => self.configured_proxy_key(key)?,
        };
        match value {
            Value::Written(value) => Ok(Resolved(value)),
            value @ Value::Cmd { on: Some(_), .. } => {
                let values = self.config_state.values();
                Ok(Resolved(values.resolve(&setting, &value).await?))
            }
            _ => bail!("{setting} is read by the process that uses it"),
        }
    }

    /// The setting of the proxy whose key is `key`, with that key, which
    /// must be one the config gives a proxy.
    fn configured_proxy_key(&self, key: Value) -> Result<(String, Value)> {
        let config = self.config();
        let (name, _) = (config.machine.proxies.iter())
            .find(|(_, proxy)| proxy.key.as_ref() == Some(&key))
            .context(
                "no [machine.proxies] entry has this key; the config changed since the agent started, so restart it",
            )?;
        Ok((format!("machine.proxies.{name}.key"), key))
    }
}

#[cfg(test)]
mod tests;
