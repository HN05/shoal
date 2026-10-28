//! Where Shoal runs work in a terminal host, recorded the same way for every
//! host so later work can find the place again. Generic code stores and
//! passes the record; only the named host reads its group, place and data.
use crate::state::states;

states!(
    /// The terminal hosts Shoal runs work in; a plain terminal is none.
    HostName: Variants {
        Terminal => "terminal",
        Herdr => "herdr",
        Rex => "rex",
        Zmx => "zmx",
    }
);

impl std::str::FromStr for HostName {
    type Err = anyhow::Error;

    fn from_str(name: &str) -> anyhow::Result<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|host| host.as_str() == name)
            .ok_or_else(|| anyhow::anyhow!("unknown terminal host {name}"))
    }
}

#[cfg(test)]
mod tests;
