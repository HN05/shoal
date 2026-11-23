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
    /// one the config writes out, which no file a launch writes holds, or
    /// one another machine prints, which only the daemon asks it for.
    pub(crate) async fn config_value(
        &self,
        space: &SpaceSelector,
        key: ValueKey,
        caller: Option<&Caller>,
    ) -> Result<Resolved> {
        let (setting, value) = match key {
            ValueKey::Secret { name, source } => {
                let secret = (self.leases)
                    .granted_secret(self, space, name.clone(), source, caller)
                    .await?;
                (format!("secrets.{name}.value"), secret.value)
            }
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
}
