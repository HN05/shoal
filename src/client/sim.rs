//! Xcode simulators as Shoal configures, leases and audits them.
use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{client::approvals::Lifetime, state::states, validate};

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    #[serde(default)]
    pub requires_approval: bool,
    #[serde(default)]
    pub approval_lifetime: Lifetime,
    pub device: String,
    /// Empty selects the latest available compatible iOS runtime.
    #[serde(default)]
    pub runtime: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SimConfig {
    pub max_booted: usize,
    pub max_devices: usize,
    pub idle_seconds: u64,
    /// Allow devices outside the configured profiles (with a reason).
    pub allow_any: bool,
    pub default: Option<String>,
    pub profiles: BTreeMap<String, Profile>,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            max_booted: 2,
            max_devices: 4,
            idle_seconds: 120,
            allow_any: false,
            default: None,
            profiles: BTreeMap::new(),
        }
    }
}

impl SimConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.max_booted > 0
                && self.max_booted <= 64
                && self.max_devices >= self.max_booted
                && self.max_devices <= 256,
            "machine.simulators limits require 1 <= max_booted <= 64 and max_booted <= max_devices <= 256"
        );
        ensure!(
            self.idle_seconds <= 86400,
            "machine.simulators.idle_seconds must be at most 86400"
        );
        if let Some(name) = &self.default {
            ensure!(
                self.profiles.contains_key(name),
                "unknown default simulator profile: {name}"
            );
        }
        for (name, profile) in &self.profiles {
            validate::name("simulator profile", name)?;
            ensure!(
                !profile.device.is_empty(),
                "simulator profile {name} requires a device"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct SimRequest {
    /// Client-generated UUID that makes clean requests idempotent and auditable.
    pub request_id: String,
    pub clean: bool,
    pub name: String,
    pub profile: Option<String>,
    pub device: Option<String>,
    pub runtime: Option<String>,
    pub reason: Option<String>,
    /// Take a place in the simulator queue while simulators are busy.
    #[serde(default)]
    pub queue: bool,
}

states!(SimulatorState {
    /// Claimed; simctl create has not returned a UDID yet.
    Creating => "creating",
    /// Claimed; booting (or erasing) for a lease.
    Booting => "booting",
    Leased => "leased",
    /// Released and reusable until the idle policy deletes it.
    Idle => "idle",
    /// A simctl step failed; release deletes the device.
    Failed => "failed",
});

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Simulator {
    pub id: String,
    pub udid: Option<String>,
    pub device: String,
    pub runtime: String,
    pub workspace_id: Option<String>,
    pub last_workspace_id: Option<String>,
    pub lease_name: Option<String>,
    pub reason: Option<String>,
    pub state: SimulatorState,
    pub last_used: u64,
    pub error: Option<String>,
    /// User apps counted at release; `None` when unknown.
    #[serde(default)]
    pub installed_apps: Option<usize>,
}

/// What could be created for a workspace: installed inventory, the policy,
/// and the repository's preferred profiles.
#[derive(Debug, Serialize, Deserialize)]
pub struct SimulatorCatalog {
    pub device_types: Vec<DeviceType>,
    pub runtimes: Vec<Runtime>,
    pub policy: SimConfig,
    pub preferred: Vec<String>,
    /// Why the installed device types and runtimes could not be read, such
    /// as a host without Xcode; the configuration is listed regardless.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory_error: Option<String>,
}

/// Managed simulators and their leases.
#[derive(Debug, Serialize, Deserialize)]
pub struct SimulatorOverview {
    pub simulators: Vec<Simulator>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DeviceType {
    pub identifier: String,
    pub name: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Runtime {
    pub identifier: String,
    pub name: String,
    pub is_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing)]
    pub supported_device_types: Option<Vec<DeviceType>>,
}

impl Runtime {
    pub fn supports(&self, device: &DeviceType) -> bool {
        self.supported_device_types
            .as_ref()
            .is_none_or(|types| types.iter().any(|d| d.identifier == device.identifier))
    }

    pub fn ios_version(&self) -> Option<Vec<u64>> {
        self.identifier
            .strip_prefix("com.apple.CoreSimulator.SimRuntime.iOS-")?;
        self.version
            .as_deref()?
            .split('.')
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()
    }
}

states!(CleanRequestStatus {
    /// Recorded; the allocation is in progress.
    Requested => "requested",
    Acquired => "acquired",
    /// No capacity; the same request ID may retry.
    Busy => "busy",
    Failed => "failed",
    /// The daemon stopped while the request was in progress.
    Interrupted => "interrupted",
});

states!(CleanAction {
    /// A fresh device was created.
    Create => "create",
    /// An existing idle device was erased.
    Erase => "erase",
    /// Idle devices were deleted to make room, then a fresh one created.
    CreateAfterEviction => "create_after_eviction",
});

#[derive(Debug, Serialize, Deserialize)]
pub struct EvictedDevice {
    pub id: String,
    pub udid: Option<String>,
    pub installed_apps: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CleanRequest {
    pub workspace_id: String,
    pub workspace_name: String,
    pub repository_id: String,
    pub execution_id: Option<String>,
    pub requested_at: u64,
    pub updated_at: u64,
    pub attempts: u64,
    pub request: SimRequest,
    pub status: CleanRequestStatus,
    pub action: Option<CleanAction>,
    pub simulator_id: Option<String>,
    pub udid: Option<String>,
    pub apps_removed: Option<usize>,
    pub erase_completed: bool,
    pub evicted: Vec<EvictedDevice>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: i64,
    #[serde(flatten)]
    pub request: CleanRequest,
}
