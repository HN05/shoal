//! Exclusive, worktree-owned Xcode simulator leases. Claims are persisted
//! before every simctl mutation so interrupted work can be reconciled.
pub mod audit;
mod planning;
pub(crate) mod records;
mod simctl;

use anyhow::{Result, ensure};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

use audit::CleanRequest;
use simctl::Inventory;

use crate::{
    daemon::{allocation::Allocation, approvals, queue, scope::Caller, workspace::Manager},
    model::Workspace,
    state::{WorkspaceState, states},
    time::unix_seconds,
    validate,
};

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    #[serde(default)]
    pub requires_approval: bool,
    #[serde(default)]
    pub approval_lifetime: crate::daemon::approvals::Lifetime,
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

impl Simulator {
    /// An unclaimed record for a device simctl has not created yet.
    fn new(profile: Profile) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            udid: None,
            device: profile.device,
            runtime: profile.runtime,
            workspace_id: None,
            last_workspace_id: None,
            lease_name: None,
            reason: None,
            state: SimulatorState::Creating,
            last_used: unix_seconds(),
            error: None,
            installed_apps: Some(0),
        }
    }

    /// Take ownership for a lease; persist it before any simctl mutation.
    fn claim(&mut self, workspace_id: &str, request: SimRequest) {
        self.workspace_id = Some(workspace_id.to_owned());
        self.lease_name = Some(request.name);
        self.reason = request.reason;
        self.state = SimulatorState::Booting;
        self.error = None;
    }

    /// Lease a booted device, or mark a failed boot while keeping the claim.
    fn finish_boot(&mut self, workspace_id: String, result: &Result<()>) {
        self.last_used = unix_seconds();
        self.last_workspace_id = Some(workspace_id);
        match result {
            Ok(()) => self.state = SimulatorState::Leased,
            Err(error) => {
                self.state = SimulatorState::Failed;
                self.error = Some(format!("{error:#}"));
            }
        }
    }

    fn is_leased_by(&self, workspace_id: &str, name: &str) -> bool {
        self.workspace_id.as_deref() == Some(workspace_id)
            && self.lease_name.as_deref() == Some(name)
    }

    fn device<'a>(&self, inventory: &'a Inventory) -> Option<&'a simctl::Device> {
        self.udid.as_deref().and_then(|udid| inventory.device(udid))
    }

    fn is_booted(&self, inventory: &Inventory) -> bool {
        self.device(inventory).is_some_and(|d| d.state.is_booted())
    }

    /// Eviction cost; unknown counts sort last.
    fn app_cost(&self) -> usize {
        self.installed_apps.unwrap_or(usize::MAX)
    }
}

/// What could be created for a workspace: installed inventory, the policy,
/// and the repository's preferred profiles.
#[derive(Debug, Serialize, Deserialize)]
pub struct SimulatorCatalog {
    pub device_types: Vec<simctl::DeviceType>,
    pub runtimes: Vec<simctl::Runtime>,
    pub policy: SimConfig,
    pub preferred: Vec<String>,
    /// Why the installed device types and runtimes could not be read, such
    /// as a host without Xcode; the configuration is listed regardless.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory_error: Option<String>,
}

/// The runtimes simctl reports as usable.
fn available(runtimes: Vec<simctl::Runtime>) -> Vec<simctl::Runtime> {
    runtimes.into_iter().filter(|r| r.is_available).collect()
}

/// Managed simulators and their leases.
#[derive(Debug, Serialize, Deserialize)]
pub struct SimulatorOverview {
    pub simulators: Vec<Simulator>,
}

impl Manager {
    pub async fn simulator_catalog(&self, selector: &str) -> Result<SimulatorCatalog> {
        let workspace = self.workspace(selector).await?;
        let config = self.config();
        let preferred = self
            .workspace_settings_from(&config, &workspace)
            .await?
            .into_settings()
            .simulators
            .preferred;
        let (device_types, runtimes, inventory_error) = match simctl::inventory().await {
            Ok(inventory) => (inventory.devicetypes, available(inventory.runtimes), None),
            Err(error) => (Vec::new(), Vec::new(), Some(format!("{error:#}"))),
        };
        Ok(SimulatorCatalog {
            device_types,
            runtimes,
            policy: config.machine.simulators.clone(),
            preferred,
            inventory_error,
        })
    }

    pub async fn simulator_overview(&self, selector: Option<&str>) -> Result<SimulatorOverview> {
        Ok(SimulatorOverview {
            simulators: self.selected_simulators(selector).await?,
        })
    }

    /// The simulators of the workspace `selector` names, or all of them.
    pub async fn selected_simulators(&self, selector: Option<&str>) -> Result<Vec<Simulator>> {
        let owner = self.workspace_filter(selector).await?;
        self.list_simulators(owner.as_deref()).await
    }

    /// Managed simulators; current ownership takes precedence over last ownership.
    pub async fn list_simulators(&self, owner: Option<&str>) -> Result<Vec<Simulator>> {
        let owner = owner.map(str::to_owned);
        self.store
            .run(move |db| records::list(db, owner.as_deref()))
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
            if !device.state.is_shutdown() {
                simctl::shutdown(&device.udid).await?;
            }
            let after = simctl::inventory().await?;
            ensure!(
                after
                    .device(&device.udid)
                    .is_none_or(|d| d.state.is_shutdown()),
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
                simctl::delete(udid).await?;
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
        caller: Option<&Caller>,
    ) -> Result<Allocation<Box<Simulator>>> {
        // Join before the gate: waiting behind another boot keeps the place.
        let workspace_id = self.workspace(selector).await?.id;
        let mut place = self.queues.enter(
            approvals::Target::Simulator,
            &workspace_id,
            &request.name,
            request.queue,
        );
        let _guard = self.gates.lock_simulators().await;
        let workspace = self.workspace(selector).await?;
        // An exported scope has no execution, but its requests still need approval.
        let scoped = caller.is_some();
        let execution_id = caller.and_then(|c| c.execution_id().map(str::to_owned));
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
            .allocate_simulator(workspace, request, &mut audit, scoped, place.ahead())
            .await;
        place.settle(&result);
        if let Ok(outcome) = &result {
            self.notify_allocation(&workspace_name, outcome, "simulator: ")
                .await;
        }
        if let Some(audit) = audit {
            self.finish_clean_request(audit, &result).await?;
        }
        result
    }

    /// All simctl transitions are serialized by the caller. Never hold a SQLite
    /// transaction across a boot; other requests must remain responsive.
    /// Earlier queued requests keep new devices busy, not existing leases.
    async fn allocate_simulator(
        &self,
        workspace: Workspace,
        request: SimRequest,
        audit: &mut Option<CleanRequest>,
        scoped: bool,
        ahead: usize,
    ) -> Result<Allocation<Box<Simulator>>> {
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
        let config = self.config();
        let limits = &config.machine.simulators;
        let mut records = self.list_simulators(None).await?;
        let inventory = simctl::inventory().await?;
        let explicit =
            request.profile.is_some() || request.device.is_some() || request.runtime.is_some();
        let existing = records
            .iter()
            .position(|s| s.is_leased_by(&workspace.id, &request.name));
        let profile = if existing.is_some() && !explicit {
            None
        } else {
            Some(
                self.resolve_profile(
                    &config,
                    &workspace,
                    &request,
                    &inventory,
                    existing.map(|index| records[index].runtime.as_str()),
                )
                .await?,
            )
        };
        if let Some(index) = existing {
            let sim = &mut records[index];
            planning::check_existing_request(sim, &request, profile.as_ref())?;
            self.reconcile_sim(sim, &inventory).await?;
            planning::check_existing_device(sim, &inventory)?;
            return Ok(Allocation::Granted(Box::new(sim.clone())));
        }
        let profile = profile.unwrap();
        if scoped && profile.requires_approval {
            let approval = approvals::ApprovalRequest::new(
                &workspace.id,
                approvals::Target::Simulator,
                &request.name,
                approvals::Specification::Simulator(approvals::SimulatorSpecification {
                    clean: request.clean,
                    profile: profile.clone(),
                }),
                profile.approval_lifetime,
                request.reason.as_deref(),
            );
            if let Some(pending) = self.request_approval(approval).await? {
                return Ok(Allocation::Approval(Box::new(pending)));
            }
        }
        if let Some(queued) = queue::behind(ahead, "simulators") {
            return Ok(queued);
        }
        // Cache live counts without booting stopped devices just to inspect them.
        if request.clean && records.len() >= limits.max_devices {
            for sim in records.iter_mut().filter(|s| s.workspace_id.is_none()) {
                if sim.is_booted(&inventory) {
                    sim.installed_apps = simctl::user_app_count(sim.udid.as_deref().unwrap())
                        .await
                        .ok();
                }
            }
        }
        let plan = planning::plan(&records, &inventory, limits, &profile, request.clean);
        self.execute_simulator_plan(&config, workspace, request, profile, plan, audit)
            .await
    }

    /// Called only after approval, under the allocation caller's simulator gate.
    /// Audits precede destructive actions; claims survive every failed boot step.
    async fn execute_simulator_plan(
        &self,
        config: &crate::config::Config,
        workspace: Workspace,
        request: SimRequest,
        profile: Profile,
        plan: planning::Plan<'_>,
        audit: &mut Option<CleanRequest>,
    ) -> Result<Allocation<Box<Simulator>>> {
        let inventory = self.recheck_plan(&plan).await?;
        if !self
            .free_running_capacity(&plan, config.machine.simulators.max_booted, inventory)
            .await?
        {
            return Ok(Allocation::Busy(
                "simulator running limit reached; active leases or external simulators occupy all slots".into(),
            ));
        }
        for candidate in &plan.evictions {
            self.evict_sim(candidate, audit).await?;
        }
        if !plan.has_device_capacity {
            return Ok(Allocation::Busy(
                "all managed simulator devices are allocated".into(),
            ));
        }
        let mut sim = plan
            .reusable
            .cloned()
            .unwrap_or_else(|| Simulator::new(profile));
        let needs_reset = request.clean && sim.udid.is_some();
        if let Some(audit) = audit.as_mut() {
            audit.record_selection(&sim, needs_reset);
            self.save_clean_request(audit).await?;
        }
        sim.claim(&workspace.id, request);
        self.save_sim(&sim).await?; // ownership precedes every external mutation
        let result = self.boot(&mut sim, &workspace, needs_reset, audit).await;
        sim.finish_boot(workspace.id, &result);
        self.save_sim(&sim).await?;
        result?;
        Ok(Allocation::Granted(Box::new(sim)))
    }

    /// A plan is not ownership evidence: confirm its devices are still unowned
    /// and the reused one still exists. Returns the inventory it checked.
    async fn recheck_plan(&self, plan: &planning::Plan<'_>) -> Result<Inventory> {
        let records = self.list_simulators(None).await?;
        for sim in plan.unowned_candidates.iter().copied().chain(plan.reusable) {
            planning::check_unowned_record(sim, &records)?;
        }
        let inventory = simctl::inventory().await?;
        if let Some(sim) = plan.reusable {
            ensure!(
                sim.device(&inventory).is_some_and(|d| d.is_available),
                "planned simulator is no longer available"
            );
        }
        Ok(inventory)
    }

    /// Shut down unowned devices until one can boot; false when none can.
    /// External devices count toward the budget but are never shut down.
    async fn free_running_capacity(
        &self,
        plan: &planning::Plan<'_>,
        max_booted: usize,
        mut inventory: Inventory,
    ) -> Result<bool> {
        for sim in &plan.unowned_candidates {
            if planning::has_running_capacity(&inventory, max_booted, plan.reusable) {
                break;
            }
            self.shutdown_sim(sim).await?;
            inventory = simctl::inventory().await?;
        }
        Ok(planning::has_running_capacity(
            &inventory,
            max_booted,
            plan.reusable,
        ))
    }

    /// Delete an idle device to make room, auditing it first for a clean request.
    async fn evict_sim(
        &self,
        candidate: &Simulator,
        audit: &mut Option<CleanRequest>,
    ) -> Result<()> {
        let mut evicted = candidate.clone();
        if let Some(audit) = audit.as_mut() {
            audit.record_eviction(&evicted);
            self.save_clean_request(audit).await?;
        }
        self.delete_sim(&mut evicted).await
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
            let udid = simctl::create(&device_name(sim), &sim.device, &sim.runtime).await?;
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
            simctl::erase(&udid).await?;
            sim.installed_apps = Some(0);
            if let Some(audit) = audit.as_mut() {
                audit.erase_completed = true;
                self.save_clean_request(audit).await?;
            }
        }
        simctl::wait_for_boot(&udid).await?;
        ensure!(
            simctl::inventory()
                .await?
                .device(&udid)
                .is_some_and(|d| d.state.is_booted() && d.is_available),
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
        config: &crate::config::Config,
        workspace: &Workspace,
        request: &SimRequest,
        inventory: &Inventory,
        existing_runtime: Option<&str>,
    ) -> Result<Profile> {
        let settings = self.workspace_settings_from(config, workspace).await?;
        planning::resolve_request(
            &config.machine.simulators,
            &settings.simulators,
            request,
            inventory,
            existing_runtime,
        )
    }

    pub async fn release_simulator(&self, selector: &str, name: String) -> Result<()> {
        let _guard = self.gates.lock_simulators().await;
        let workspace = self.workspace(selector).await?;
        ensure!(
            workspace.state == WorkspaceState::Ready,
            "workspace is not ready"
        );
        self.touch(&workspace.id).await;
        let sim = self
            .list_simulators(Some(&workspace.id))
            .await?
            .into_iter()
            .find(|s| s.is_leased_by(&workspace.id, &name));
        let Some(mut sim) = sim else {
            ensure!(
                self.release_simulator_access(workspace.id, name).await?,
                "unknown simulator lease or approval request"
            );
            return Ok(());
        };
        if sim.state != SimulatorState::Leased {
            self.delete_sim(&mut sim).await?;
            self.release_simulator_access(workspace.id, name).await?;
            return Ok(());
        }
        sim.installed_apps = match sim.udid.as_deref() {
            Some(udid) => simctl::user_app_count(udid).await.ok(),
            None => None,
        };
        sim.last_workspace_id = sim.workspace_id.take();
        sim.lease_name = None;
        sim.state = SimulatorState::Idle;
        sim.last_used = unix_seconds();
        self.save_sim(&sim).await?;
        self.release_simulator_access(workspace.id, name).await?;
        Ok(())
    }

    async fn release_simulator_access(&self, owner: String, name: String) -> Result<bool> {
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let released =
                    approvals::release(&tx, &owner, &approvals::Target::Simulator, &name)?;
                tx.commit()?;
                Ok(released)
            })
            .await
    }

    pub async fn remove_simulators(&self, owner: &str) -> Result<()> {
        let _guard = self.gates.lock_simulators().await;
        for mut sim in self.list_simulators(Some(owner)).await? {
            self.delete_sim(&mut sim).await?;
        }
        Ok(())
    }

    /// Delete expired idle devices to bound both storage and file count.
    /// Compatible devices can be reused during the grace period.
    pub async fn expire_simulators(&self) -> Result<()> {
        let Some(_guard) = self.gates.try_lock_simulators() else {
            return Ok(());
        };
        let idle_seconds = self.config().machine.simulators.idle_seconds;
        for mut sim in self.list_simulators(None).await? {
            if sim.workspace_id.is_none()
                && unix_seconds().saturating_sub(sim.last_used) >= idle_seconds
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
