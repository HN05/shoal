//! Exclusive, worktree-owned Xcode simulator leases. Claims are persisted
//! before every simctl mutation so interrupted work can be reconciled.
use anyhow::{Result, bail, ensure};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

use crate::{
    model::Workspace,
    sim_audit::{CleanAction, CleanRequest, CleanRequestStatus, EvictedDevice},
    simctl::{self, Inventory},
    state::{WorkspaceState, states},
    validate,
    workspace::Manager,
};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub device: String,
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
            "simulators limits require 1 <= max_booted <= 64 and max_booted <= max_devices <= 256"
        );
        ensure!(
            self.idle_seconds <= 86400,
            "simulators.idle_seconds must be at most 86400"
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
                !profile.device.is_empty() && !profile.runtime.is_empty(),
                "simulator profile {name} requires a device and runtime"
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

impl Simulator {
    fn is_leased_by(&self, workspace_id: &str, name: &str) -> bool {
        self.workspace_id.as_deref() == Some(workspace_id)
            && self.lease_name.as_deref() == Some(name)
    }

    fn device<'a>(&self, inventory: &'a Inventory) -> Option<&'a simctl::Device> {
        self.udid.as_deref().and_then(|udid| inventory.device(udid))
    }

    fn is_booted(&self, inventory: &Inventory) -> bool {
        self.device(inventory).is_some_and(|d| d.state == "Booted")
    }

    /// Eviction cost; unknown counts sort last.
    fn app_cost(&self) -> usize {
        self.installed_apps.unwrap_or(usize::MAX)
    }
}

/// What `shoal sim catalog` shows: what could be created, and the policy.
#[derive(Debug, Serialize, Deserialize)]
pub struct SimulatorCatalog {
    pub device_types: Vec<simctl::DeviceType>,
    pub runtimes: Vec<simctl::Runtime>,
    pub policy: SimConfig,
}

pub enum Acquisition {
    Acquired(Box<Simulator>),
    Busy(String),
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Manager {
    pub async fn simulator_catalog(&self) -> Result<SimulatorCatalog> {
        let inventory = simctl::inventory().await?;
        Ok(SimulatorCatalog {
            device_types: inventory.devicetypes,
            runtimes: inventory
                .runtimes
                .into_iter()
                .filter(|r| r.is_available)
                .collect(),
            policy: self.config.simulators.clone(),
        })
    }

    /// Managed simulators, optionally those currently or last owned by a workspace.
    pub async fn list_simulators(&self, owner: Option<&str>) -> Result<Vec<Simulator>> {
        let owner = owner.map(str::to_owned);
        self.store
            .run(move |db| {
                let records = db
                    .prepare("SELECT record FROM simulators ORDER BY id")?
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut records = records
                    .into_iter()
                    .map(|r| serde_json::from_str::<Simulator>(&r))
                    .collect::<serde_json::Result<Vec<_>>>()?;
                if let Some(owner) = owner {
                    records.retain(|s| {
                        s.workspace_id.as_ref().or(s.last_workspace_id.as_ref()) == Some(&owner)
                    });
                }
                Ok(records)
            })
            .await
    }

    async fn save_sim(&self, sim: &Simulator) -> Result<()> {
        let (id, record) = (sim.id.clone(), serde_json::to_string(sim)?);
        self.store
            .run(move |db| {
                db.execute(
                    "INSERT INTO simulators(id,record) VALUES (?1,?2) ON CONFLICT(id) DO UPDATE SET record=excluded.record",
                    params![id, record],
                )?;
                Ok(())
            })
            .await
    }

    /// Resolve a creation interrupted between simctl returning and saving its UDID.
    async fn reconcile_sim(&self, sim: &mut Simulator, inventory: &Inventory) -> Result<()> {
        if sim.udid.is_none() {
            let expected = device_name(sim);
            if let Some(device) = inventory
                .devices
                .values()
                .flatten()
                .find(|d| d.name == expected)
            {
                sim.udid = Some(device.udid.clone());
                self.save_sim(sim).await?;
            }
        }
        Ok(())
    }

    async fn shutdown_sim(&self, sim: &Simulator) -> Result<()> {
        let inventory = simctl::inventory().await?;
        if let Some(device) = sim.device(&inventory) {
            if device.state != "Shutdown" {
                simctl::run(&["shutdown", &device.udid]).await?;
            }
            let after = simctl::inventory().await?;
            ensure!(
                after
                    .device(&device.udid)
                    .is_none_or(|d| d.state == "Shutdown"),
                "simulator {} has not shut down",
                device.udid
            );
        }
        Ok(())
    }

    async fn delete_sim(&self, sim: &mut Simulator) -> Result<()> {
        let inventory = simctl::inventory().await?;
        self.reconcile_sim(sim, &inventory).await?;
        self.shutdown_sim(sim).await?;
        if let Some(udid) = &sim.udid {
            if inventory.device(udid).is_some() {
                simctl::run(&["delete", udid]).await?;
            }
            ensure!(
                simctl::inventory().await?.device(udid).is_none(),
                "simulator still exists after deletion"
            );
        }
        let id = sim.id.clone();
        self.store
            .run(move |db| {
                db.execute("DELETE FROM simulators WHERE id=?1", [id])?;
                Ok(())
            })
            .await
    }

    pub async fn acquire_simulator(
        &self,
        selector: &str,
        request: SimRequest,
        execution_id: Option<String>,
    ) -> Result<Acquisition> {
        let _guard = self.simulator_gate.lock().await;
        let workspace = self.workspace(selector).await?;
        let mut audit = if request.clean {
            Some(
                self.start_clean_request(&workspace, &request, execution_id)
                    .await?,
            )
        } else {
            None
        };
        let workspace_name = workspace.name.clone();
        let result = self
            .allocate_simulator(workspace, request, &mut audit)
            .await;
        if let Ok(Acquisition::Busy(message)) = &result {
            self.notify(
                Some(&workspace_name),
                crate::notifications::NotificationKind::ResourceBusy,
                format!("simulator: {message}"),
            )
            .await;
        }
        if let Some(mut audit) = audit {
            audit.updated_at = now();
            match &result {
                Ok(Acquisition::Acquired(sim)) => {
                    audit.status = CleanRequestStatus::Acquired;
                    audit.simulator_id = Some(sim.id.clone());
                    audit.udid = sim.udid.clone();
                }
                Ok(Acquisition::Busy(message)) => {
                    audit.status = CleanRequestStatus::Busy;
                    audit.error = Some(message.clone());
                }
                Err(error) => {
                    audit.status = CleanRequestStatus::Failed;
                    audit.error = Some(format!("{error:#}"));
                }
            }
            self.save_clean_request(&audit).await?;
        }
        result
    }

    /// All simctl transitions are serialized by the caller. Never hold a SQLite
    /// transaction across a boot; other requests must remain responsive.
    async fn allocate_simulator(
        &self,
        workspace: Workspace,
        request: SimRequest,
        audit: &mut Option<CleanRequest>,
    ) -> Result<Acquisition> {
        validate::name("simulator lease", &request.name)?;
        validate::reason("simulator", request.reason.as_deref())?;
        ensure!(
            !request.clean || request.reason.is_some(),
            "--clean requires --reason explaining why a clean device is necessary"
        );
        ensure!(
            workspace.state == WorkspaceState::Ready,
            "workspace is not ready"
        );
        self.touch(&workspace.id).await;
        let limits = &self.config.simulators;
        let mut records = self.list_simulators(None).await?;
        let mut inventory = simctl::inventory().await?;
        let explicit =
            request.profile.is_some() || request.device.is_some() || request.runtime.is_some();
        let existing = records
            .iter()
            .position(|s| s.is_leased_by(&workspace.id, &request.name));
        let profile = if existing.is_some() && !explicit {
            None
        } else {
            Some(
                self.resolve_profile(&workspace, &request, &inventory)
                    .await?,
            )
        };
        if let Some(index) = existing {
            ensure!(
                !request.clean,
                "this simulator name is already leased; release it before requesting a clean device, or use another --name"
            );
            let sim = &mut records[index];
            if let Some(profile) = &profile {
                ensure!(
                    sim.device == profile.device && sim.runtime == profile.runtime,
                    "simulator name already leased with different settings; release it first"
                );
            }
            self.reconcile_sim(sim, &inventory).await?;
            ensure!(
                sim.state == SimulatorState::Leased,
                "simulator allocation is {}; release it to clean up and retry",
                sim.state
            );
            ensure!(
                sim.device(&inventory)
                    .is_some_and(|d| d.is_available && d.state == "Booted"),
                "leased simulator is no longer booted/available; release it and acquire again"
            );
            return Ok(Acquisition::Acquired(Box::new(sim.clone())));
        }
        let profile = profile.unwrap();
        // Cache live counts without booting stopped devices just to inspect them.
        if request.clean && records.len() >= limits.max_devices {
            for sim in records.iter_mut().filter(|s| s.workspace_id.is_none()) {
                if sim.is_booted(&inventory) {
                    sim.installed_apps = simctl::user_app_count(sim.udid.as_deref().unwrap())
                        .await
                        .ok();
                }
            }
            records.sort_by_key(|s| (s.app_cost(), s.last_used));
        } else {
            records.sort_by_key(|s| std::cmp::Reverse(s.last_used));
        }
        let mut reusable = records
            .iter()
            .find(|s| {
                s.workspace_id.is_none()
                    && s.state == SimulatorState::Idle
                    && s.device == profile.device
                    && s.runtime == profile.runtime
                    && s.device(&inventory).is_some_and(|d| d.is_available)
            })
            .cloned();
        if request.clean {
            // Spare capacity costs zero reinstalls. At capacity, replacing an
            // incompatible empty device can cost less than erasing a useful one.
            let evictions_needed = records.len().saturating_sub(limits.max_devices) + 1;
            let eviction_candidates: Vec<_> = records
                .iter()
                .filter(|s| s.workspace_id.is_none())
                .take(evictions_needed)
                .collect();
            let eviction_cost = eviction_candidates
                .iter()
                .fold(0usize, |cost, s| cost.saturating_add(s.app_cost()));
            let cheaper_eviction = eviction_candidates.len() == evictions_needed
                && reusable
                    .as_ref()
                    .is_some_and(|r| eviction_cost < r.app_cost());
            if records.len() < limits.max_devices || cheaper_eviction {
                reusable = None;
            }
        }
        let reuse_running = reusable.as_ref().is_some_and(|s| s.is_booted(&inventory));
        let mut idle: Vec<_> = records
            .iter()
            .filter(|s| s.workspace_id.is_none() && reusable.as_ref().is_none_or(|r| r.id != s.id))
            .cloned()
            .collect();
        if request.clean {
            idle.sort_by_key(|s| (s.app_cost(), s.last_used));
        } else {
            idle.sort_by_key(|s| s.last_used);
        }
        // External devices count toward the budget but are never shut down.
        for sim in &idle {
            if inventory.running_count() < limits.max_booted
                || (reuse_running && inventory.running_count() <= limits.max_booted)
            {
                break;
            }
            self.shutdown_sim(sim).await?;
            inventory = simctl::inventory().await?;
        }
        if inventory.running_count() >= limits.max_booted
            && !(reuse_running && inventory.running_count() == limits.max_booted)
        {
            return Ok(Acquisition::Busy(
                "simulator running limit reached; active leases or external simulators occupy all slots".into(),
            ));
        }
        if reusable.is_none() {
            let mut count = records.len();
            let mut candidates = idle.iter().cloned();
            while count >= limits.max_devices {
                let Some(mut oldest) = candidates.next() else {
                    return Ok(Acquisition::Busy(
                        "all managed simulator devices are allocated".into(),
                    ));
                };
                if let Some(audit) = audit.as_mut() {
                    audit.evicted.push(EvictedDevice {
                        id: oldest.id.clone(),
                        udid: oldest.udid.clone(),
                        installed_apps: oldest.installed_apps,
                    });
                    audit.action = Some(CleanAction::CreateAfterEviction);
                    self.save_clean_request(audit).await?;
                }
                self.delete_sim(&mut oldest).await?;
                count -= 1;
            }
        }
        let mut sim = reusable.unwrap_or_else(|| Simulator {
            id: Uuid::new_v4().to_string(),
            udid: None,
            device: profile.device,
            runtime: profile.runtime,
            workspace_id: None,
            last_workspace_id: None,
            lease_name: None,
            reason: None,
            state: SimulatorState::Creating,
            last_used: now(),
            error: None,
            installed_apps: Some(0),
        });
        let needs_reset = request.clean && sim.udid.is_some();
        if let Some(audit) = audit.as_mut() {
            audit.simulator_id = Some(sim.id.clone());
            audit.udid = sim.udid.clone();
            audit.apps_removed = needs_reset.then_some(sim.installed_apps).flatten();
            if audit.action.is_none() {
                audit.action = Some(if needs_reset {
                    CleanAction::Erase
                } else {
                    CleanAction::Create
                });
            }
            self.save_clean_request(audit).await?;
        }
        sim.workspace_id = Some(workspace.id.clone());
        sim.lease_name = Some(request.name);
        sim.reason = request.reason;
        sim.state = SimulatorState::Booting;
        sim.error = None;
        self.save_sim(&sim).await?; // ownership precedes every external mutation
        let result = self.boot(&mut sim, &workspace, needs_reset, audit).await;
        sim.last_used = now();
        sim.last_workspace_id = Some(workspace.id);
        if let Err(error) = result {
            sim.state = SimulatorState::Failed;
            sim.error = Some(format!("{error:#}"));
            self.save_sim(&sim).await?;
            return Err(error);
        }
        sim.state = SimulatorState::Leased;
        self.save_sim(&sim).await?;
        Ok(Acquisition::Acquired(Box::new(sim)))
    }

    /// Create the device if needed, optionally erase it, and boot it.
    async fn boot(
        &self,
        sim: &mut Simulator,
        workspace: &Workspace,
        needs_reset: bool,
        audit: &mut Option<CleanRequest>,
    ) -> Result<()> {
        if sim.udid.is_none() {
            let udid =
                simctl::run(&["create", &device_name(sim), &sim.device, &sim.runtime]).await?;
            Uuid::parse_str(&udid)?;
            sim.udid = Some(udid);
            self.save_sim(sim).await?;
            if let Some(audit) = audit.as_mut() {
                audit.udid = sim.udid.clone();
                self.save_clean_request(audit).await?;
            }
        }
        let udid = sim.udid.clone().unwrap();
        if needs_reset {
            self.shutdown_sim(sim).await?;
            simctl::run(&["erase", &udid]).await?;
            sim.installed_apps = Some(0);
            if let Some(audit) = audit.as_mut() {
                audit.erase_completed = true;
                self.save_clean_request(audit).await?;
            }
        }
        simctl::run(&["bootstatus", &udid, "-b"]).await?;
        ensure!(
            simctl::inventory()
                .await?
                .device(&udid)
                .is_some_and(|d| d.state == "Booted" && d.is_available),
            "simulator did not finish booting"
        );
        // Removal may have started while simctl was running. Its cleanup waits
        // for our gate, so retain the claim and let that path remove it.
        ensure!(
            self.workspace(&workspace.id).await?.state == WorkspaceState::Ready,
            "workspace removal started during simulator boot"
        );
        Ok(())
    }

    async fn resolve_profile(
        &self,
        workspace: &Workspace,
        request: &SimRequest,
        inventory: &Inventory,
    ) -> Result<Profile> {
        let config = &self.config.simulators;
        ensure!(
            request.profile.is_none() || (request.device.is_none() && request.runtime.is_none()),
            "use either --profile or --device with --runtime"
        );
        let resolve = |profile: &Profile| resolve_profile(inventory, profile);
        let profile = if request.device.is_some() || request.runtime.is_some() {
            let (Some(device), Some(runtime)) = (&request.device, &request.runtime) else {
                bail!("--device and --runtime are required together");
            };
            Profile {
                device: device.clone(),
                runtime: runtime.clone(),
            }
        } else {
            let repo = self.workspace_config(workspace).await?;
            let names = if let Some(name) = &request.profile {
                vec![name.clone()]
            } else if !repo.simulators.preferred.is_empty() {
                repo.simulators.preferred
            } else {
                config.default.iter().cloned().collect()
            };
            ensure!(
                !names.is_empty(),
                "select --profile or configure simulators.default and simulators.profiles in global config"
            );
            let mut candidates = Vec::new();
            for name in names {
                candidates.push(
                    config
                        .profiles
                        .get(&name)
                        .ok_or_else(|| anyhow::anyhow!("unknown simulator profile: {name}"))?,
                );
            }
            let mut failures = Vec::new();
            let found = candidates
                .into_iter()
                .find_map(|candidate| match resolve(candidate) {
                    Ok(profile) => Some(profile),
                    Err(error) => {
                        failures.push(error.to_string());
                        None
                    }
                });
            found.ok_or_else(|| {
                anyhow::anyhow!(
                    "no preferred simulator is available: {}",
                    failures.join("; ")
                )
            })?
        };
        let profile = resolve(&profile)?;
        let allowed = config
            .profiles
            .values()
            .filter_map(|p| resolve(p).ok())
            .any(|p| p.device == profile.device && p.runtime == profile.runtime);
        ensure!(
            allowed || config.allow_any,
            "simulator is not in the machine's allowed profiles"
        );
        ensure!(
            allowed || request.reason.is_some(),
            "requesting a simulator outside configured profiles requires --reason"
        );
        Ok(profile)
    }

    pub async fn release_simulator(&self, selector: &str, name: String) -> Result<()> {
        let _guard = self.simulator_gate.lock().await;
        let workspace = self.workspace(selector).await?;
        ensure!(
            workspace.state == WorkspaceState::Ready,
            "workspace is not ready"
        );
        self.touch(&workspace.id).await;
        let mut sim = self
            .list_simulators(Some(&workspace.id))
            .await?
            .into_iter()
            .find(|s| s.is_leased_by(&workspace.id, &name))
            .ok_or_else(|| anyhow::anyhow!("unknown simulator lease: {name}"))?;
        if sim.state != SimulatorState::Leased {
            return self.delete_sim(&mut sim).await;
        }
        sim.installed_apps = match sim.udid.as_deref() {
            Some(udid) => simctl::user_app_count(udid).await.ok(),
            None => None,
        };
        sim.last_workspace_id = sim.workspace_id.take();
        sim.lease_name = None;
        sim.state = SimulatorState::Idle;
        sim.last_used = now();
        self.save_sim(&sim).await
    }

    pub async fn remove_simulators(&self, owner: &str) -> Result<()> {
        let _guard = self.simulator_gate.lock().await;
        for mut sim in self.list_simulators(Some(owner)).await? {
            self.delete_sim(&mut sim).await?;
        }
        Ok(())
    }

    /// Delete expired idle devices to bound both storage and file count.
    /// Compatible devices can be reused during the grace period.
    pub async fn expire_simulators(&self) -> Result<()> {
        let Ok(_guard) = self.simulator_gate.try_lock() else {
            return Ok(());
        };
        for mut sim in self.list_simulators(None).await? {
            if sim.workspace_id.is_none()
                && now().saturating_sub(sim.last_used) >= self.config.simulators.idle_seconds
            {
                self.delete_sim(&mut sim).await?;
            }
        }
        Ok(())
    }
}

/// The simctl device name Shoal gives its own devices.
fn device_name(sim: &Simulator) -> String {
    format!("shoal-{}", sim.id)
}

/// Match a profile's device/runtime names or identifiers against what is
/// installed, returning canonical identifiers.
fn resolve_profile(inventory: &Inventory, profile: &Profile) -> Result<Profile> {
    let devices: Vec<_> = inventory
        .devicetypes
        .iter()
        .filter(|d| d.identifier == profile.device || d.name == profile.device)
        .collect();
    let runtimes: Vec<_> = inventory
        .runtimes
        .iter()
        .filter(|r| {
            r.is_available && (r.identifier == profile.runtime || r.name == profile.runtime)
        })
        .collect();
    ensure!(
        devices.len() == 1,
        "device type is unavailable or ambiguous: {}",
        profile.device
    );
    ensure!(
        runtimes.len() == 1,
        "runtime is not installed/available or is ambiguous: {}; Shoal does not download runtimes",
        profile.runtime
    );
    ensure!(
        runtimes[0]
            .supported_device_types
            .as_ref()
            .is_none_or(|types| types.iter().any(|d| d.identifier == devices[0].identifier)),
        "device type {} is incompatible with runtime {}",
        profile.device,
        profile.runtime
    );
    Ok(Profile {
        device: devices[0].identifier.clone(),
        runtime: runtimes[0].identifier.clone(),
    })
}
