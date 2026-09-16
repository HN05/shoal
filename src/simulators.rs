use anyhow::{Result, bail, ensure};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

use crate::{repo_config, simctl, workspace::Manager};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub device: String,
    pub runtime: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SimConfig {
    pub max_booted: usize,
    pub max_devices: usize,
    pub idle_seconds: u64,
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
            crate::workspace::validate_name(name)?;
            ensure!(
                !profile.device.is_empty() && !profile.runtime.is_empty(),
                "simulator profile {name} requires a device and runtime"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SimRequest {
    pub name: String,
    pub profile: Option<String>,
    pub device: Option<String>,
    pub runtime: Option<String>,
    pub reason: Option<String>,
}

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
    pub state: String,
    pub last_used: u64,
    pub error: Option<String>,
}

pub enum Acquisition {
    Acquired(Box<Simulator>),
    Busy(String),
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Manager {
    pub async fn simulators(&self, owner: Option<String>) -> Result<Vec<Simulator>> {
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
        self.store.run(move |db| { db.execute("INSERT INTO simulators(id,record) VALUES (?1,?2) ON CONFLICT(id) DO UPDATE SET record=excluded.record", params![id,record])?; Ok(()) }).await
    }

    /// Resolve a creation interrupted between simctl returning and saving its UDID.
    async fn reconcile_sim(
        &self,
        sim: &mut Simulator,
        inventory: &simctl::Inventory,
    ) -> Result<()> {
        if sim.udid.is_none() {
            let expected = format!("shoal-{}", sim.id);
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
        if let Some(device) = sim.udid.as_deref().and_then(|u| inventory.device(u)) {
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
        selector: String,
        request: SimRequest,
    ) -> Result<Acquisition> {
        crate::workspace::validate_name(&request.name)?;
        ensure!(
            request.reason.as_ref().is_none_or(|s| !s.trim().is_empty()
                && s.len() <= 256
                && !s.contains(['\n', '\r'])),
            "simulator reason must be a nonempty single line (max 256 bytes)"
        );
        // All simctl transitions are serialized. Never hold a SQLite transaction
        // across a boot; other resource/workspace requests remain responsive.
        let _guard = self.sim_gate.lock().await;
        let workspace = self.get(selector).await?;
        ensure!(workspace.state == "ready", "workspace is not ready");
        self.touch(&workspace.id).await;
        let mut records = self.simulators(None).await?;
        let mut inventory = simctl::inventory().await?;
        let explicit =
            request.profile.is_some() || request.device.is_some() || request.runtime.is_some();
        let existing = records.iter().position(|s| {
            s.workspace_id.as_ref() == Some(&workspace.id)
                && s.lease_name.as_ref() == Some(&request.name)
        });
        let profile = if existing.is_some() && !explicit {
            None
        } else {
            Some(self.resolve_profile(&workspace.path, &request, &inventory)?)
        };
        if let Some(index) = existing {
            let sim = &mut records[index];
            if let Some(profile) = &profile {
                ensure!(
                    sim.device == profile.device && sim.runtime == profile.runtime,
                    "simulator name already leased with different settings; release it first"
                );
            }
            self.reconcile_sim(sim, &inventory).await?;
            ensure!(
                sim.state == "leased",
                "simulator allocation is {}; release it to clean up and retry",
                sim.state
            );
            ensure!(
                sim.udid
                    .as_deref()
                    .and_then(|u| inventory.device(u))
                    .is_some_and(|d| d.is_available && d.state == "Booted"),
                "leased simulator is no longer booted/available; release it and acquire again"
            );
            return Ok(Acquisition::Acquired(Box::new(sim.clone())));
        }
        let profile = profile.unwrap();
        // Prefer an idle compatible device, most recently used first (warm cache).
        records.sort_by_key(|s| std::cmp::Reverse(s.last_used));
        let reusable = records
            .iter()
            .find(|s| {
                s.workspace_id.is_none()
                    && s.state == "idle"
                    && s.device == profile.device
                    && s.runtime == profile.runtime
                    && s.udid
                        .as_deref()
                        .and_then(|u| inventory.device(u))
                        .is_some_and(|d| d.is_available)
            })
            .cloned();
        let reuse_running = reusable
            .as_ref()
            .and_then(|s| s.udid.as_deref())
            .and_then(|u| inventory.device(u))
            .is_some_and(|d| d.state == "Booted");
        let mut idle: Vec<_> = records
            .iter()
            .filter(|s| s.workspace_id.is_none() && reusable.as_ref().is_none_or(|r| r.id != s.id))
            .cloned()
            .collect();
        idle.sort_by_key(|s| s.last_used);
        // External devices count toward the budget but are never shut down.
        for sim in &idle {
            if inventory.running_count() < self.config.simulators.max_booted
                || (reuse_running && inventory.running_count() <= self.config.simulators.max_booted)
            {
                break;
            }
            self.shutdown_sim(sim).await?;
            inventory = simctl::inventory().await?;
        }
        if inventory.running_count() >= self.config.simulators.max_booted
            && !(reuse_running && inventory.running_count() == self.config.simulators.max_booted)
        {
            return Ok(Acquisition::Busy("simulator running limit reached; active leases or external simulators occupy all slots".into()));
        }
        if reusable.is_none() {
            let mut count = records.len();
            let mut candidates = idle.iter().cloned();
            while count >= self.config.simulators.max_devices {
                let Some(mut oldest) = candidates.next() else {
                    return Ok(Acquisition::Busy(
                        "all managed simulator devices are allocated".into(),
                    ));
                };
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
            state: "creating".into(),
            last_used: now(),
            error: None,
        });
        let needs_reset =
            sim.udid.is_some() && sim.last_workspace_id.as_ref() != Some(&workspace.id);
        sim.workspace_id = Some(workspace.id.clone());
        sim.lease_name = Some(request.name);
        sim.reason = request.reason;
        sim.state = "booting".into();
        sim.error = None;
        self.save_sim(&sim).await?; // ownership precedes every external mutation
        let result = async {
            if sim.udid.is_none() {
                let udid = simctl::run(&[
                    "create",
                    &format!("shoal-{}", sim.id),
                    &sim.device,
                    &sim.runtime,
                ])
                .await?;
                Uuid::parse_str(&udid)?;
                sim.udid = Some(udid);
                self.save_sim(&sim).await?;
            }
            let udid = sim.udid.clone().unwrap();
            if needs_reset {
                self.shutdown_sim(&sim).await?;
                simctl::run(&["erase", &udid]).await?;
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
                self.get(workspace.id.clone()).await?.state == "ready",
                "workspace removal started during simulator boot"
            );
            Ok::<(), anyhow::Error>(())
        }
        .await;
        sim.last_used = now();
        sim.last_workspace_id = Some(workspace.id);
        if let Err(error) = result {
            sim.state = "failed".into();
            sim.error = Some(format!("{error:#}"));
            self.save_sim(&sim).await?;
            return Err(error);
        }
        sim.state = "leased".into();
        self.save_sim(&sim).await?;
        Ok(Acquisition::Acquired(Box::new(sim)))
    }

    fn resolve_profile(
        &self,
        path: &std::path::Path,
        request: &SimRequest,
        inventory: &simctl::Inventory,
    ) -> Result<Profile> {
        let config = &self.config.simulators;
        ensure!(
            request.profile.is_none() || (request.device.is_none() && request.runtime.is_none()),
            "use either --profile or --device with --runtime"
        );
        let resolve = |profile: &Profile| -> Result<Profile> {
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
                    .is_none_or(|types| types
                        .iter()
                        .any(|d| d.identifier == devices[0].identifier)),
                "device type {} is incompatible with runtime {}",
                profile.device,
                profile.runtime
            );
            Ok(Profile {
                device: devices[0].identifier.clone(),
                runtime: runtimes[0].identifier.clone(),
            })
        };
        let profile = if request.device.is_some() || request.runtime.is_some() {
            let (Some(device), Some(runtime)) = (&request.device, &request.runtime) else {
                bail!("--device and --runtime are required together");
            };
            Profile {
                device: device.clone(),
                runtime: runtime.clone(),
            }
        } else {
            let repo = repo_config::load(path)?;
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

    pub async fn release_simulator(&self, selector: String, name: String) -> Result<()> {
        let _guard = self.sim_gate.lock().await;
        let workspace = self.get(selector).await?;
        ensure!(workspace.state == "ready", "workspace is not ready");
        self.touch(&workspace.id).await;
        let mut sim = self
            .simulators(Some(workspace.id.clone()))
            .await?
            .into_iter()
            .find(|s| {
                s.workspace_id.as_ref() == Some(&workspace.id)
                    && s.lease_name.as_ref() == Some(&name)
            })
            .ok_or_else(|| anyhow::anyhow!("unknown simulator lease: {name}"))?;
        if sim.state != "leased" {
            return self.delete_sim(&mut sim).await;
        }
        sim.last_workspace_id = sim.workspace_id.take();
        sim.lease_name = None;
        sim.state = "idle".into();
        sim.last_used = now();
        self.save_sim(&sim).await
    }

    pub async fn remove_simulators(&self, owner: &str) -> Result<()> {
        let _guard = self.sim_gate.lock().await;
        for mut sim in self.simulators(Some(owner.into())).await? {
            self.delete_sim(&mut sim).await?;
        }
        Ok(())
    }

    pub async fn expire_simulators(&self) -> Result<()> {
        let Ok(_guard) = self.sim_gate.try_lock() else {
            return Ok(());
        };
        for mut sim in self.simulators(None).await? {
            if sim.workspace_id.is_none()
                && now().saturating_sub(sim.last_used) >= self.config.simulators.idle_seconds
            {
                // Delete expired idle devices to bound both storage and file count.
                // Compatible devices can be reused during the grace period.
                self.delete_sim(&mut sim).await?;
            }
        }
        Ok(())
    }
}
