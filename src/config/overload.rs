//! Machine-wide protection for the daemon's tracked agents.
use anyhow::{Result, ensure};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Overload {
    pub memory: Memory,
    pub cpu: Cpu,
    pub cooldown_seconds: u64,
    pub poll_seconds: u64,
}

impl Default for Overload {
    fn default() -> Self {
        Self {
            memory: Memory::default(),
            cpu: Cpu::default(),
            cooldown_seconds: 5,
            poll_seconds: 2,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Memory {
    pub enabled: bool,
    /// Linux MemAvailable includes reclaimable cache; macOS uses native pressure.
    pub used_percent: u8,
    pub sustained_seconds: u64,
}

impl Default for Memory {
    fn default() -> Self {
        Self {
            enabled: true,
            used_percent: 95,
            sustained_seconds: 0,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cpu {
    pub enabled: bool,
    pub used_percent: u8,
    pub sustained_seconds: u64,
}

impl Default for Cpu {
    fn default() -> Self {
        Self {
            enabled: false,
            used_percent: 90,
            sustained_seconds: 300,
        }
    }
}

impl Overload {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (50..=99).contains(&self.memory.used_percent),
            "overload.memory.used_percent must be between 50 and 99"
        );
        ensure!(
            self.memory.sustained_seconds <= 86400,
            "overload.memory.sustained_seconds must be between 0 and 86400"
        );
        ensure!(
            (1..=100).contains(&self.cpu.used_percent),
            "overload.cpu.used_percent must be between 1 and 100"
        );
        validate_seconds(self.cpu.sustained_seconds, "overload.cpu.sustained_seconds")?;
        validate_seconds(self.poll_seconds, "overload.poll_seconds")?;
        validate_seconds(self.cooldown_seconds, "overload.cooldown_seconds")
    }
}

fn validate_seconds(value: u64, key: &str) -> Result<()> {
    ensure!(
        (1..=86400).contains(&value),
        "{key} must be between 1 and 86400"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_is_opt_out_and_settings_are_bounded() {
        let defaults = Overload::default();
        assert!(defaults.memory.enabled);
        assert!(!defaults.cpu.enabled);
        defaults.validate().unwrap();
        for text in [
            "cooldown_seconds = 0",
            "[cpu]\nused_percent = 0",
            "[cpu]\nused_percent = 101",
            "[cpu]\nsustained_seconds = 0",
            "[memory]\nused_percent = 0",
            "[memory]\nused_percent = 100",
            "[memory]\nsustained_seconds = 86401",
        ] {
            let config: Overload = toml::from_str(text).unwrap();
            assert!(config.validate().is_err(), "{text}");
        }
        let config: Overload = toml::from_str("[memory]\nenabled = false").unwrap();
        assert!(!config.memory.enabled);
        assert_eq!(config.memory.used_percent, defaults.memory.used_percent);
        assert!(toml::from_str::<Overload>("[memory]\nunknown = true").is_err());
        assert!(crate::config::repo::parse("[overload.memory]\nenabled = false").is_err());
    }
}
