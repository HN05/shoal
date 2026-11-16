//! A length of time as config writes it: whole numbers each followed by a
//! unit, `s`, `m`, `h`, `d` or `w`, largest first, such as `90s`, `10m` or
//! `1h30m`.
use std::{fmt, str::FromStr, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

/// A positive length of time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Span(Duration);

const UNITS: [(char, u64); 5] = [
    ('w', 7 * 24 * 3600),
    ('d', 24 * 3600),
    ('h', 3600),
    ('m', 60),
    ('s', 1),
];

impl Span {
    pub const fn from_secs(seconds: u64) -> Self {
        Self(Duration::from_secs(seconds))
    }

    pub fn duration(self) -> Duration {
        self.0
    }
}

impl FromStr for Span {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let mut seconds = 0u64;
        let mut rest = text.trim();
        let mut previous: Option<usize> = None;
        ensure!(
            !rest.is_empty(),
            "expected a length of time such as 10m or 1h30m"
        );
        while !rest.is_empty() {
            let digits = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            ensure!(
                digits > 0,
                "{text}: expected a number before each unit, such as 10m"
            );
            let count: u64 = rest[..digits].parse().context("the number is too large")?;
            let unit = rest[digits..].chars().next();
            let Some(index) = UNITS.iter().position(|(name, _)| Some(*name) == unit) else {
                bail!("{text}: each number needs a unit, s, m, h, d or w");
            };
            ensure!(
                previous.is_none_or(|previous| index > previous),
                "{text}: give the units largest first, once each"
            );
            previous = Some(index);
            seconds = count
                .checked_mul(UNITS[index].1)
                .and_then(|part| seconds.checked_add(part))
                .context("the length of time is too large")?;
            rest = &rest[digits + 1..];
        }
        ensure!(seconds > 0, "{text}: a length of time must be positive");
        Ok(Self::from_secs(seconds))
    }
}

/// The largest units first, as config writes it.
impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut rest = self.0.as_secs();
        for (unit, seconds) in UNITS {
            if rest >= seconds {
                write!(f, "{}{unit}", rest / seconds)?;
                rest %= seconds;
            }
        }
        Ok(())
    }
}

impl Serialize for Span {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Span {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests;
