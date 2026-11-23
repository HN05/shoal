//! The values of settings that a process of a space asks the daemon for,
//! as ones it cannot read itself.
use anyhow::{Result, bail};

use crate::daemon::{manager::Manager, scope::Caller};
use shoal_protocol::{
    config::value::{Resolved, Value, ValueKey},
    space_names::SpaceSelector,
};

impl Manager {
    /// The value of `key` for the space `space`, as `caller` may read it:
    /// one the config writes out, which no file a launch writes holds.
    pub(crate) async fn config_value(
        &self,
        space: &SpaceSelector,
        key: ValueKey,
        caller: Option<&Caller>,
    ) -> Result<Resolved> {
        let setting = key.to_string();
        let value = match key {
            ValueKey::Secret { name, source } => {
                (self.leases)
                    .granted_secret(self, space, name, source, caller)
                    .await?
                    .value
            }
        };
        match value {
            Value::Written(value) => Ok(Resolved(value)),
            _ => bail!("{setting} is read by the process that uses it"),
        }
    }
}
