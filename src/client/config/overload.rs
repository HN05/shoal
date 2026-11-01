//! Machine-wide protection for the daemon's tracked agents.
use anyhow::{Result, ensure};
use serde::Deserialize;

crate::state::states! {
    /// How hard Shoal may load this machine: a computer someone works on, or
    /// one that only runs agents. It sets the CPU and memory thresholds that
    /// `[machine.overload]` leaves out.
    #[derive(Default)]
    Load {
        Light => "light",
        #[default]
        Balanced => "balanced",
        Full => "full",
    }
}

/// Warning, stop and recovery percentages for one signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Levels {
    warning: u8,
    stop: u8,
    recovery: u8,
}

impl Levels {
    const fn new(warning: u8, stop: u8, recovery: u8) -> Self {
        Self {
            warning,
            stop,
            recovery,
        }
    }
}

impl Load {
    fn cpu(self) -> Levels {
        match self {
            Self::Light => Levels::new(60, 80, 55),
            Self::Balanced => Levels::new(80, 90, 75),
            Self::Full => Levels::new(95, 100, 90),
        }
    }

    /// Swap gives memory room to fill further before the kernel kills
    /// processes. Only Linux reads these; macOS uses native pressure.
    fn memory(self, swap: bool) -> Levels {
        match (self, swap) {
            (Self::Light, false) => Levels::new(75, 90, 70),
            (Self::Light, true) => Levels::new(80, 93, 75),
            (Self::Balanced, false) => Levels::new(85, 95, 80),
            (Self::Balanced, true) => Levels::new(90, 97, 85),
            (Self::Full, false) => Levels::new(95, 98, 90),
            (Self::Full, true) => Levels::new(97, 99, 93),
        }
    }
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Overload {
    pub memory: Memory,
    pub cpu: Cpu,
    pub disk: Disk,
    pub recovery: Recovery,
    pub warning: Warning,
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
            warning: Warning::default(),
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
            used_percent: Load::default().memory(false).stop,
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
            used_percent: Load::default().cpu().stop,
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

    pub fn stop_free_bytes(&self) -> u64 {
        self.stop_free_gib << 30
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
            memory_used_percent: Load::default().memory(false).recovery,
            cpu_used_percent: Load::default().cpu().recovery,
            sustained_seconds: 60,
        }
    }
}

/// Thresholds, normally below each enabled protection's stop threshold, at
/// which Shoal messages running agents so they can reduce load first.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Warning {
    pub enabled: bool,
    /// Linux only; macOS warns at native warning pressure.
    pub memory_used_percent: u8,
    pub cpu_used_percent: u8,
    pub cpu_sustained_seconds: u64,
    pub disk_free_gib: u64,
    /// The least time between warnings about one signal to one workspace.
    pub repeat_minutes: u64,
}

impl Default for Warning {
    fn default() -> Self {
        Self {
            enabled: true,
            memory_used_percent: Load::default().memory(false).warning,
            cpu_used_percent: Load::default().cpu().warning,
            cpu_sustained_seconds: 60,
            disk_free_gib: 10,
            repeat_minutes: 30,
        }
    }
}

impl Warning {
    pub fn disk_free_bytes(&self) -> u64 {
        self.disk_free_gib << 30
    }
}

impl Overload {
    /// Set the CPU and memory thresholds `written`, the `[machine.overload]` table as
    /// the file spells it, leaves out from `load`.
    pub(crate) fn follow_load(&mut self, load: Load, swap: bool, written: &toml::Table) {
        let unset = |table: &str, key: &str| {
            written
                .get(table)
                .and_then(toml::Value::as_table)
                .is_none_or(|table| !table.contains_key(key))
        };
        let (cpu, memory) = (load.cpu(), load.memory(swap));
        for (table, key, value, level) in [
            (
                "memory",
                "used_percent",
                &mut self.memory.used_percent,
                memory.stop,
            ),
            ("cpu", "used_percent", &mut self.cpu.used_percent, cpu.stop),
            (
                "warning",
                "memory_used_percent",
                &mut self.warning.memory_used_percent,
                memory.warning,
            ),
            (
                "warning",
                "cpu_used_percent",
                &mut self.warning.cpu_used_percent,
                cpu.warning,
            ),
            (
                "recovery",
                "memory_used_percent",
                &mut self.recovery.memory_used_percent,
                memory.recovery,
            ),
            (
                "recovery",
                "cpu_used_percent",
                &mut self.recovery.cpu_used_percent,
                cpu.recovery,
            ),
        ] {
            if unset(table, key) {
                *value = level;
            }
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            (50..=99).contains(&self.memory.used_percent),
            "machine.overload.memory.used_percent must be between 50 and 99"
        );
        ensure!(
            self.memory.sustained_seconds <= 86400,
            "machine.overload.memory.sustained_seconds must be between 0 and 86400"
        );
        ensure!(
            (1..=100).contains(&self.cpu.used_percent),
            "machine.overload.cpu.used_percent must be between 1 and 100"
        );
        validate_seconds(
            self.cpu.sustained_seconds,
            "machine.overload.cpu.sustained_seconds",
        )?;
        ensure!(
            self.recovery.memory_used_percent > 0
                && self.recovery.memory_used_percent < self.memory.used_percent,
            "machine.overload.recovery.memory_used_percent must be positive and below machine.overload.memory.used_percent"
        );
        ensure!(
            self.recovery.cpu_used_percent > 0
                && self.recovery.cpu_used_percent < self.cpu.used_percent,
            "machine.overload.recovery.cpu_used_percent must be positive and below machine.overload.cpu.used_percent"
        );
        ensure!(
            (1..=MAX_FREE_GIB).contains(&self.disk.stop_free_gib),
            "machine.overload.disk.stop_free_gib must be between 1 and {MAX_FREE_GIB}"
        );
        ensure!(
            (self.disk.stop_free_gib..=MAX_FREE_GIB).contains(&self.disk.cleanup_free_gib),
            "machine.overload.disk.cleanup_free_gib must be between overload.disk.stop_free_gib and {MAX_FREE_GIB}"
        );
        validate_seconds(
            self.recovery.sustained_seconds,
            "machine.overload.recovery.sustained_seconds",
        )?;
        // Warnings are not checked against stop thresholds, so existing
        // configurations stay valid; a warning at or above one comes with the stop.
        ensure!(
            (1..=99).contains(&self.warning.memory_used_percent),
            "machine.overload.warning.memory_used_percent must be between 1 and 99"
        );
        ensure!(
            (1..=100).contains(&self.warning.cpu_used_percent),
            "machine.overload.warning.cpu_used_percent must be between 1 and 100"
        );
        validate_seconds(
            self.warning.cpu_sustained_seconds,
            "machine.overload.warning.cpu_sustained_seconds",
        )?;
        ensure!(
            (1..=MAX_FREE_GIB).contains(&self.warning.disk_free_gib),
            "machine.overload.warning.disk_free_gib must be between 1 and {MAX_FREE_GIB}"
        );
        ensure!(
            (1..=1440).contains(&self.warning.repeat_minutes),
            "machine.overload.warning.repeat_minutes must be between 1 and 1440"
        );
        validate_seconds(self.poll_seconds, "machine.overload.poll_seconds")?;
        validate_seconds(self.cooldown_seconds, "machine.overload.cooldown_seconds")
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
mod tests;
