//! `[machine] memory_limit`: the most memory Shoal's work may hold on this
//! machine together, a size or a share of its RAM, and the limit in effect
//! where the config leaves it to `load`.
use std::fmt;

use serde::{Deserialize, Serialize};
use shoal_protocol::protocol::MemoryLimitSource;

use super::overload::Load;

/// The binary units a size takes, with the bytes each stands for.
const UNITS: [(&str, u64); 5] = [
    ("B", 1),
    ("KiB", 1 << 10),
    ("MiB", 1 << 20),
    ("GiB", 1 << 30),
    ("TiB", 1 << 40),
];

/// The most memory Shoal's work may hold on this machine together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLimit {
    /// This many bytes, written as a size such as `24GiB`.
    Bytes(u64),
    /// This share of the machine's RAM, in percent, from 1 to 100.
    Percent(u8),
}

impl MemoryLimit {
    /// The limit in bytes on a machine with `ram` bytes of RAM.
    pub fn bytes(self, ram: u64) -> u64 {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::Percent(percent) => (u128::from(ram) * u128::from(percent) / 100)
                .try_into()
                .unwrap_or(u64::MAX),
        }
    }

    /// The limit `text` writes, as the config spells it.
    fn parse(text: &str) -> Result<Self, String> {
        let invalid = || {
            format!(
                "memory_limit must be a size such as \"24GiB\", a share of RAM such as \"75%\" or \"none\", not {text:?}"
            )
        };
        if let Some(percent) = text.strip_suffix('%') {
            let percent: u8 = percent.trim().parse().map_err(|_| invalid())?;
            return match percent {
                1..=100 => Ok(Self::Percent(percent)),
                _ => Err("memory_limit's share of RAM must be from 1% to 100%".into()),
            };
        }
        let split = text
            .find(|character: char| !character.is_ascii_digit())
            .ok_or_else(invalid)?;
        let (number, unit) = text.split_at(split);
        let number: u64 = number.parse().map_err(|_| invalid())?;
        let (_, scale) = (UNITS.iter())
            .find(|(name, _)| *name == unit.trim())
            .ok_or_else(invalid)?;
        match number.checked_mul(*scale) {
            Some(0) => Err("memory_limit must be more than 0 bytes".into()),
            Some(bytes) => Ok(Self::Bytes(bytes)),
            None => Err(invalid()),
        }
    }
}

impl fmt::Display for MemoryLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Percent(percent) => write!(formatter, "{percent}%"),
            Self::Bytes(bytes) => {
                let (unit, scale) = (UNITS.iter().rev())
                    .find(|(_, scale)| bytes % scale == 0)
                    .expect("every size is whole bytes");
                write!(formatter, "{}{unit}", bytes / scale)
            }
        }
    }
}

impl Serialize for MemoryLimit {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for MemoryLimit {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// `memory_limit` as the config writes it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MemoryLimitSetting {
    /// Left out: the share of RAM `load` sets, where the kernel enforces it.
    #[default]
    Load,
    /// `"none"`: no limit.
    Off,
    /// A size or a share of RAM.
    Limit(MemoryLimit),
}

/// The limit on Shoal's work in effect, and what sets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InEffect {
    /// None without a limit.
    pub limit: Option<MemoryLimit>,
    pub source: MemoryLimitSource,
}

impl MemoryLimitSetting {
    /// Whether the config leaves the limit to `load`.
    pub fn follows_load(&self) -> bool {
        *self == Self::Load
    }

    /// The limit in effect on this machine under `load`.
    pub fn in_effect(self, load: Load) -> InEffect {
        self.where_enforced(load, enforced())
    }

    /// The limit in effect under `load`, where the kernel `enforced` a
    /// limit or not. Without enforcement a default would only make
    /// admission count less memory, so only a limit the config sets holds.
    fn where_enforced(self, load: Load, enforced: bool) -> InEffect {
        let (limit, source) = match self {
            Self::Load if enforced => (
                Some(MemoryLimit::Percent(load.work_memory_percent())),
                MemoryLimitSource::Load,
            ),
            Self::Load => (None, MemoryLimitSource::Load),
            Self::Off => (None, MemoryLimitSource::Config),
            Self::Limit(limit) => (Some(limit), MemoryLimitSource::Config),
        };
        InEffect { limit, source }
    }
}

/// Whether the kernel enforces a limit on Shoal's work for this build: on
/// Linux, in a build that caps the work slice, which a development build
/// leaves to the installed Shoal.
fn enforced() -> bool {
    cfg!(target_os = "linux") && crate::build::changes_user_files()
}

impl Serialize for MemoryLimitSetting {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Load => serializer.serialize_none(),
            Self::Off => serializer.serialize_str(OFF),
            Self::Limit(limit) => limit.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for MemoryLimitSetting {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text == OFF {
            return Ok(Self::Off);
        }
        MemoryLimit::parse(&text)
            .map(Self::Limit)
            .map_err(serde::de::Error::custom)
    }
}

/// The value that turns the limit off.
const OFF: &str = "none";

#[cfg(test)]
mod tests;
