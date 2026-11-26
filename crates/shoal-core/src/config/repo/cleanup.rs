//! The `[cleanup]` section and its subsections.
use crate::config::options::Options;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Cleanup)]
pub struct CleanupConfig {
    #[option(default = 10)]
    pub grace_minutes: Option<u64>,
    #[option(section)]
    pub auto: AutoCleanupConfig,
    #[option(section)]
    pub pr: PrCleanupConfig,
    #[option(section)]
    pub merged: MergedCleanupConfig,
    #[option(section)]
    pub restore: RestoreConfig,
    #[option(section)]
    pub done: CleanupDoneConfig,
}

impl CleanupConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(minutes) = self.grace_minutes {
            anyhow::ensure!(
                minutes <= 525600,
                "cleanup.grace_minutes must be at most 525600"
            );
        }
        for (key, minutes) in [
            ("cleanup.auto.idle_minutes", self.auto.idle_minutes),
            ("cleanup.merged.idle_minutes", self.merged.idle_minutes),
        ] {
            if let Some(minutes) = minutes {
                crate::config::validate_idle_minutes(key, minutes)?;
            }
        }
        if let Some(days) = self.restore.retention_days {
            anyhow::ensure!(
                (1..=3650).contains(&days),
                "cleanup.restore.retention_days must be between 1 and 3650"
            );
        }
        if let Some(megabytes) = self.restore.archive_max_mb {
            anyhow::ensure!(
                megabytes <= 10240,
                "cleanup.restore.archive_max_mb must be at most 10240"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = AutoCleanup)]
pub struct AutoCleanupConfig {
    #[option(default = true)]
    pub enabled: Option<bool>,
    #[option(default = 10)]
    pub idle_minutes: Option<u64>,
}

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = MergedCleanup)]
pub struct MergedCleanupConfig {
    #[option(default = false)]
    pub enabled: Option<bool>,
    #[option(default = 240)]
    pub idle_minutes: Option<u64>,
}

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Restore)]
pub struct RestoreConfig {
    #[option(default = 30)]
    pub retention_days: Option<u64>,
    #[option(default = 50)]
    pub archive_max_mb: Option<u64>,
}

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = CleanupDone)]
pub struct CleanupDoneConfig {
    #[option(default = true)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = PrCleanup)]
pub struct PrCleanupConfig {
    #[option(default = true)]
    pub enabled: Option<bool>,
}
