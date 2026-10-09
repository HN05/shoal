//! Machine-wide protection for the daemon's tracked agents.
use anyhow::{Result, ensure};
use serde::Deserialize;

#[derive(Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Overload {
    pub memory: Memory,
    pub cpu: Cpu,
    pub disk: Disk,
    pub recovery: Recovery,
    pub cooldown_seconds: u64,
    pub poll_seconds: u64,
}

impl Default for Overload {
    fn default() -> Self {
        Self {
            memory: Memory::default(),
            cpu: Cpu::default(),
            disk: Disk::default(),
            recovery: Recovery::default(),
            cooldown_seconds: 5,
            poll_seconds: 2,
        }
    }
}

#[derive(Debug, PartialEq, Deserialize)]
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

#[derive(Debug, PartialEq, Deserialize)]
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

/// Free space, in GiB, on filesystems holding workspaces or daemon state.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Disk {
    pub enabled: bool,
    /// Remove workspaces idle cleanup would remove, without the idle delay.
    pub cleanup_free_gib: u64,
    /// Stop tracked executions when cleanup cannot free enough space.
    pub stop_free_gib: u64,
}

impl Default for Disk {
    fn default() -> Self {
        Self {
            enabled: true,
            cleanup_free_gib: 5,
            stop_free_gib: 2,
        }
    }
}

impl Disk {
    pub fn cleanup_free_bytes(&self) -> u64 {
        self.cleanup_free_gib << 30
    }
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Recovery {
    pub enabled: bool,
    pub memory_used_percent: u8,
    pub cpu_used_percent: u8,
    pub sustained_seconds: u64,
}

impl Default for Recovery {
    fn default() -> Self {
        Self {
            enabled: true,
            memory_used_percent: 85,
            cpu_used_percent: 75,
            sustained_seconds: 60,
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
        ensure!(
            self.recovery.memory_used_percent > 0
                && self.recovery.memory_used_percent < self.memory.used_percent,
            "overload.recovery.memory_used_percent must be positive and below overload.memory.used_percent"
        );
        ensure!(
            self.recovery.cpu_used_percent > 0
                && self.recovery.cpu_used_percent < self.cpu.used_percent,
            "overload.recovery.cpu_used_percent must be positive and below overload.cpu.used_percent"
        );
        ensure!(
            (1..=MAX_FREE_GIB).contains(&self.disk.stop_free_gib),
            "overload.disk.stop_free_gib must be between 1 and {MAX_FREE_GIB}"
        );
        ensure!(
            (self.disk.stop_free_gib..=MAX_FREE_GIB).contains(&self.disk.cleanup_free_gib),
            "overload.disk.cleanup_free_gib must be between overload.disk.stop_free_gib and {MAX_FREE_GIB}"
        );
        validate_seconds(
            self.recovery.sustained_seconds,
            "overload.recovery.sustained_seconds",
        )?;
        validate_seconds(self.poll_seconds, "overload.poll_seconds")?;
        validate_seconds(self.cooldown_seconds, "overload.cooldown_seconds")
    }
}

/// One PiB keeps byte thresholds far from overflow.
const MAX_FREE_GIB: u64 = 1 << 20;

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
        assert!(defaults.disk.enabled);
        defaults.validate().unwrap();
        for text in [
            "cooldown_seconds = 0",
            "[recovery]\nmemory_used_percent = 95",
            "[recovery]\ncpu_used_percent = 90",
            "[recovery]\nsustained_seconds = 0",
            "[cpu]\nused_percent = 0",
            "[cpu]\nused_percent = 101",
            "[cpu]\nsustained_seconds = 0",
            "[memory]\nused_percent = 0",
            "[memory]\nused_percent = 100",
            "[memory]\nsustained_seconds = 86401",
            "[disk]\nstop_free_gib = 0",
            "[disk]\ncleanup_free_gib = 1",
            "[disk]\ncleanup_free_gib = 1048577",
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
