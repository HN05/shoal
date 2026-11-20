//! `[machine] memory_limit`: the most memory Shoal's work may hold on this
//! machine together, a size or a share of its RAM.
use std::fmt;

use serde::{Deserialize, Serialize};

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
                "memory_limit must be a size such as \"24GiB\" or a share of RAM such as \"75%\", not {text:?}"
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

#[cfg(test)]
mod tests;
