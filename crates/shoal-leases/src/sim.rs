//! Exclusive, worktree-owned Xcode simulator leases. Claims are persisted
//! before every simctl mutation so interrupted work can be reconciled.
use crate::sim::audit::CleanRequestExt;
mod audit;
mod planning;
mod records;
mod simctl;

use anyhow::{Result, ensure};
use rusqlite::params;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use uuid::Uuid;

use audit::CleanRequest;
pub use records::interrupt_clean_requests;
pub use shoal_protocol::sim::{
    Profile, SimConfig, SimRequest, Simulator, SimulatorCatalog, SimulatorOverview, SimulatorState,
};
use simctl::Inventory;

use crate::{
    Leases, acquired_leases::Acquired, allocation::Allocation, callers::Caller, daemon::Daemon,
    model::Workspace, queue, state::WorkspaceState, store, time::UnixTime, validate,
};
use shoal_daemon_base::approval_requests::ApprovalRequestExt;
use shoal_protocol::approvals;
use shoal_protocol::{
    acquirers::Acquirer,
    demos::{Launch, Preset, SIM_UDID},
    space_names::SpaceSelector,
};

/// Operations on the client type [`Simulator`] that only the `shoal` command uses.
trait SimulatorExt {
    /// An unclaimed record for a device simctl has not created yet.
    fn new(profile: Profile) -> Self;

    /// Take ownership for a lease; persist it before any simctl mutation.
    fn claim(&mut self, workspace_id: &SpaceId, request: SimRequest, acquirer: Acquirer);

    /// Lease a booted device, or mark a failed boot while keeping the claim.
    fn finish_boot(&mut self, workspace_id: SpaceId, result: &Result<()>);

    fn is_leased_by(&self, workspace_id: &SpaceId, name: &str) -> bool;

    fn device<'a>(&self, inventory: &'a Inventory) -> Option<&'a simctl::Device>;

    fn is_booted(&self, inventory: &Inventory) -> bool;

    /// Eviction cost; unknown counts sort last.
    fn app_cost(&self) -> usize;
}

impl SimulatorExt for Simulator {
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
            last_used: UnixTime::now(),
            error: None,
            installed_apps: Some(0),
            acquirer: Acquirer::User,
        }
    }

    fn claim(&mut self, workspace_id: &SpaceId, request: SimRequest, acquirer: Acquirer) {
        self.workspace_id = Some(workspace_id.to_owned());
        self.lease_name = Some(request.name);
        self.reason = request.reason;
        self.acquirer = acquirer;
        self.state = SimulatorState::Booting;
        self.error = None;
    }

    fn finish_boot(&mut self, workspace_id: SpaceId, result: &Result<()>) {
        self.last_used = UnixTime::now();
        self.last_workspace_id = Some(workspace_id);
        match result {
            Ok(()) => self.state = SimulatorState::Leased,
            Err(error) => {
                self.state = SimulatorState::Failed;
                self.error = Some(format!("{error:#}"));
            }
        }
    }

    fn is_leased_by(&self, workspace_id: &SpaceId, name: &str) -> bool {
        self.workspace_id.as_ref() == Some(workspace_id) && self.lease_name.as_deref() == Some(name)
    }

    fn device<'a>(&self, inventory: &'a Inventory) -> Option<&'a simctl::Device> {
        self.udid.as_deref().and_then(|udid| inventory.device(udid))
    }

    fn is_booted(&self, inventory: &Inventory) -> bool {
        self.device(inventory).is_some_and(|d| d.state.is_booted())
    }

    fn app_cost(&self) -> usize {
        self.installed_apps.unwrap_or(usize::MAX)
    }
}

/// What an allocation came to, with the limit that keeps it busy where
/// one does.
struct SimClaim {
    allocation: Allocation<Box<Simulator>>,
    full: Option<Limit>,
}

impl From<Allocation<Box<Simulator>>> for SimClaim {
    fn from(allocation: Allocation<Box<Simulator>>) -> Self {
        Self {
            allocation,
            full: None,
        }
    }
}

/// A limit on simulators that keeps a request busy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Limit {
    /// `max_booted`, which booted devices count against.
    Booted,
    /// `max_devices`, which every device counts against.
    Devices,
}

/// The request asking for a simulator, as allocation weighs it.
#[derive(Clone)]
struct Claimant {
    /// Whose lease it becomes.
    acquirer: Acquirer,
    /// A scoped caller's request, which takes configured approvals.
    approves: bool,
    /// Queued requests that arrived earlier.
    ahead: usize,
}

/// The runtimes simctl reports as usable.
fn available(runtimes: Vec<simctl::Runtime>) -> Vec<simctl::Runtime> {
    runtimes.into_iter().filter(|r| r.is_available).collect()
}

impl Leases {
    pub async fn simulator_catalog(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<SimulatorCatalog> {
        let workspace = daemon.workspace(selector).await?;
        let config = self.config.current();
        let preferred = daemon
            .settings(&config, &workspace)
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

    pub async fn simulator_overview(
        &self,
        daemon: &dyn Daemon,
        selector: Option<&SpaceSelector>,
    ) -> Result<SimulatorOverview> {
        Ok(SimulatorOverview {
            simulators: self.selected_simulators(daemon, selector).await?,
        })
    }

    /// The simulators of the workspace `selector` names, or all of them.
    pub async fn selected_simulators(
        &self,
        daemon: &dyn Daemon,
        selector: Option<&SpaceSelector>,
    ) -> Result<Vec<Simulator>> {
        let owner = self.workspace_filter(daemon, selector).await?;
        self.list_simulators(owner.as_ref()).await
    }

    /// Managed simulators; current ownership takes precedence over last ownership.
    pub async fn list_simulators(&self, owner: Option<&SpaceId>) -> Result<Vec<Simulator>> {
        let owner = owner.cloned();
        self.store
            .run(move |db| records::list(db, owner.as_ref()))
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
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        request: SimRequest,
        caller: Option<&Caller>,
    ) -> Result<Allocation<Box<Simulator>>> {
        // Join before the gate: waiting behind another boot keeps the place.
        let workspace_id = daemon.workspace(selector).await?.id;
        let mut place = self.queues.enter(
            approvals::Target::Simulator,
            &workspace_id,
            &request.name,
            request.queue,
        );
        let _guard = self.gates.lock_simulators().await;
        let workspace = daemon.workspace(selector).await?;
        let execution_id = caller.and_then(|c| c.execution_id().cloned());
        let mut audit = if request.clean {
            Some(
                self.start_clean_request(&workspace, &request, execution_id)
                    .await?,
            )
        } else {
            None
        };
        let workspace_name = (workspace.name.clone(), workspace.id.clone());
        let claimant = Claimant {
            acquirer: self.acquirer(daemon, caller).await,
            approves: caller.is_some(),
            ahead: place.ahead(),
        };
        let mut result = self
            .allocate_simulator(
                daemon,
                workspace.clone(),
                request.clone(),
                &mut audit,
                claimant.clone(),
            )
            .await;
        // A device asleep agents hold goes to the request, which allocates
        // again under the same lock.
        let full = result.as_ref().ok().and_then(|claim| claim.full);
        if let Some(limit) = full
            && self
                .give_simulator_to_request(daemon, &workspace, caller, limit)
                .await
        {
            result = self
                .allocate_simulator(daemon, workspace, request, &mut audit, claimant)
                .await;
        }
        let result = result.map(|claim| claim.allocation);
        place.settle(&result);
        if let Ok(outcome) = &result {
            let space = (&workspace_name.0, &workspace_name.1);
            (self.notify_allocation(daemon, space, outcome, "simulator: ")).await;
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
        daemon: &dyn Daemon,
        workspace: Workspace,
        request: SimRequest,
        audit: &mut Option<CleanRequest>,
        claimant: Claimant,
    ) -> Result<SimClaim> {
        validate::name("simulator lease", &request.name)?;
        validate::reason("simulator", request.reason.as_deref())?;
        ensure!(
            !request.clean || request.reason.is_some(),
            "--clean requires --reason explaining why a clean device is necessary"
        );
        store::ensure_ready(&workspace)?;
        daemon.touch(&workspace.id).await;
        let config = self.config.current();
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
                    daemon,
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
            let renewed = sim.acquirer.renewed_by(&claimant.acquirer);
            if renewed != sim.acquirer {
                sim.acquirer = renewed;
                self.save_sim(sim).await?;
            }
            return Ok(Allocation::Granted(Box::new(sim.clone())).into());
        }
        let profile = profile.unwrap();
        // An exported scope has no execution, but its requests still need approval.
        if claimant.approves && profile.requires_approval {
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
                return Ok(Allocation::Approval(Box::new(pending)).into());
            }
        }
        if let Some(queued) = queue::behind(claimant.ahead, "simulators") {
            return Ok(queued.into());
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
        let claim = (request, claimant.acquirer);
        self.execute_simulator_plan(daemon, &config, workspace, claim, profile, plan, audit)
            .await
    }

    /// Called only after approval, under the allocation caller's simulator gate.
    /// Audits precede destructive actions; claims survive every failed boot step.
    #[allow(
        clippy::too_many_arguments,
        reason = "the plan and what it acts for, which only allocation passes"
    )]
    async fn execute_simulator_plan(
        &self,
        daemon: &dyn Daemon,
        config: &crate::config::Config,
        workspace: Workspace,
        (request, acquirer): (SimRequest, Acquirer),
        profile: Profile,
        plan: planning::Plan<'_>,
        audit: &mut Option<CleanRequest>,
    ) -> Result<SimClaim> {
        let inventory = self.recheck_plan(&plan).await?;
        if !self
            .free_running_capacity(&plan, config.machine.simulators.max_booted, inventory)
            .await?
        {
            return Ok(SimClaim {
                allocation: Allocation::Busy(
                    "simulator running limit reached; active leases or external simulators occupy all slots".into(),
                ),
                full: Some(Limit::Booted),
            });
        }
        for candidate in &plan.evictions {
            self.evict_sim(candidate, audit).await?;
        }
        if !plan.has_device_capacity {
            return Ok(SimClaim {
                allocation: Allocation::Busy("all managed simulator devices are allocated".into()),
                full: Some(Limit::Devices),
            });
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
        sim.claim(&workspace.id, request, acquirer);
        self.save_sim(&sim).await?; // ownership precedes every external mutation
        let result = (self.boot(daemon, &mut sim, &workspace, needs_reset, audit)).await;
        sim.finish_boot(workspace.id, &result);
        self.save_sim(&sim).await?;
        result?;
        Ok(Allocation::Granted(Box::new(sim)).into())
    }

    /// Give a request for `workspace` from `caller`, which `limit` keeps
    /// busy, a simulator lease an asleep agent holds that frees it: a booted
    /// device for the running limit, any device for the device limit, as
    /// [`Self::give_to_request`] picks one. The caller holds the simulators'
    /// lock. Whether one went.
    async fn give_simulator_to_request(
        &self,
        daemon: &dyn Daemon,
        workspace: &Workspace,
        caller: Option<&Caller>,
        limit: Limit,
    ) -> bool {
        let freeing = match self.freeing_simulators(limit).await {
            Ok(freeing) => freeing,
            Err(error) => {
                crate::log!("simulators unread for a busy request: {error:#}");
                return false;
            }
        };
        (self.give_to_request(daemon, workspace, caller, freeing, Device::ForRequest)).await
    }

    /// Each leased simulator whose release frees what `limit` keeps busy,
    /// alone.
    async fn freeing_simulators(&self, limit: Limit) -> Result<Vec<Vec<Acquired>>> {
        let inventory = match limit {
            Limit::Booted => Some(simctl::inventory().await?),
            Limit::Devices => None,
        };
        let frees = |sim: &Simulator| {
            sim.state == SimulatorState::Leased
                && inventory
                    .as_ref()
                    .is_none_or(|inventory| sim.is_booted(inventory))
        };
        let simulators = self.list_simulators(None).await?.into_iter();
        Ok((simulators.filter(frees))
            .filter_map(Acquired::simulator)
            .map(|lease| vec![lease])
            .collect())
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
        daemon: &dyn Daemon,
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
            daemon.workspace_by_id(&workspace.id).await?.state == WorkspaceState::Ready,
            "workspace removal started during simulator boot"
        );
        Ok(())
    }

    async fn resolve_profile(
        &self,
        daemon: &dyn Daemon,
        config: &crate::config::Config,
        workspace: &Workspace,
        request: &SimRequest,
        inventory: &Inventory,
        existing_runtime: Option<&str>,
    ) -> Result<Profile> {
        let settings = daemon.settings(config, workspace).await?;
        planning::resolve_request(
            &config.machine.simulators,
            &settings.simulators,
            request,
            inventory,
            existing_runtime,
        )
    }

    pub async fn release_simulator(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        name: String,
    ) -> Result<()> {
        (self.release_simulator_held_by(daemon, selector, name, None))
            .await
            .map(drop)
    }

    /// Release the simulator lease `name` of `selector`'s workspace, unless
    /// `holder` names an acquirer it no longer has, as when the user took
    /// over an agent's lease since a release of it was decided. Only a
    /// release without `holder`, which someone asked for, is activity.
    /// Whether it released the lease.
    pub async fn release_simulator_held_by(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        name: String,
        holder: Option<&Acquirer>,
    ) -> Result<bool> {
        (self.release_simulator_then(daemon, selector, name, holder, Device::Keep)).await
    }

    /// [`Self::release_simulator_held_by`], then do `then` with its device.
    pub(crate) async fn release_simulator_then(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        name: String,
        holder: Option<&Acquirer>,
        then: Device,
    ) -> Result<bool> {
        let guard = match then {
            // Its request holds the lock already.
            Device::ForRequest => None,
            Device::Keep | Device::ShutDown => Some(self.gates.lock_simulators().await),
        };
        let released = (self.release_locked_simulator(daemon, selector, name, holder)).await?;
        let SimRelease::Released(idle) = released else {
            return Ok(false);
        };
        if let Some(sim) = idle {
            if then == Device::ShutDown && sim.udid.is_some() {
                self.shutdown_sim(&sim).await?;
            }
            // Expiry passes over the devices while another holds the lock.
            drop(guard);
            self.simulator_idle.notify_one();
        }
        Ok(true)
    }

    /// Release the simulator lease `name` of `selector`'s workspace as
    /// [`Self::release_simulator_held_by`] does, under the simulators' lock
    /// the caller holds.
    pub(crate) async fn release_locked_simulator(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        name: String,
        holder: Option<&Acquirer>,
    ) -> Result<SimRelease> {
        let workspace = daemon.workspace(selector).await?;
        store::ensure_ready(&workspace)?;
        if holder.is_none() {
            daemon.touch(&workspace.id).await;
        }
        let sim = self
            .list_simulators(Some(&workspace.id))
            .await?
            .into_iter()
            .find(|s| s.is_leased_by(&workspace.id, &name));
        if let Some(holder) = holder
            && sim.as_ref().is_none_or(|sim| sim.acquirer != *holder)
        {
            return Ok(SimRelease::Kept);
        }
        let Some(mut sim) = sim else {
            ensure!(
                self.release_simulator_access(workspace.id, name).await?,
                "unknown simulator lease or approval request"
            );
            return Ok(SimRelease::Released(None));
        };
        if sim.state != SimulatorState::Leased {
            self.delete_sim(&mut sim).await?;
            self.release_simulator_access(workspace.id, name).await?;
            return Ok(SimRelease::Released(None));
        }
        sim.installed_apps = match sim.udid.as_deref() {
            Some(udid) => simctl::user_app_count(udid).await.ok(),
            None => None,
        };
        sim.last_workspace_id = sim.workspace_id.take();
        sim.lease_name = None;
        sim.acquirer = Acquirer::User;
        sim.state = SimulatorState::Idle;
        sim.last_used = UnixTime::now();
        self.save_sim(&sim).await?;
        self.release_simulator_access(workspace.id, name).await?;
        Ok(SimRelease::Released(Some(Box::new(sim))))
    }

    async fn release_simulator_access(&self, owner: SpaceId, name: String) -> Result<bool> {
        let records = self.records.clone();
        self.store
            .write(move |tx| {
                records.release_approval(tx, &owner, &approvals::Target::Simulator, &name)
            })
            .await
    }

    /// Give the demo `execution` the simulator lease of the device it runs
    /// on, as `launch` names it, when an agent acquired it for the demo, so
    /// the lease ends with the demo rather than with the agent. The user's
    /// lease stays the user's, and another demo's that demo's.
    pub async fn give_simulator_to_demo(
        &self,
        execution: &ExecutionId,
        workspace: &SpaceId,
        launch: &Launch,
    ) -> Result<()> {
        if launch.preset != Some(Preset::Simulator) {
            return Ok(());
        }
        let Some(udid) = launch.environment.get(SIM_UDID) else {
            return Ok(());
        };
        let _guard = self.gates.lock_simulators().await;
        let leased = self.list_simulators(Some(workspace)).await?.into_iter();
        let agents = leased.filter(|sim| {
            sim.workspace_id.as_ref() == Some(workspace)
                && sim.udid.as_ref() == Some(udid)
                && matches!(sim.acquirer, Acquirer::Agent { .. })
        });
        for mut sim in agents {
            sim.acquirer = Acquirer::Demo {
                execution_id: execution.to_owned(),
            };
            self.save_sim(&sim).await?;
        }
        Ok(())
    }

    /// Make the simulator lease `name` of `workspace` the user's.
    pub async fn give_simulator_lease_to_user(
        &self,
        workspace: &SpaceId,
        name: &str,
    ) -> Result<()> {
        let _guard = self.gates.lock_simulators().await;
        let leased = self.list_simulators(Some(workspace)).await?.into_iter();
        if let Some(mut sim) = leased.into_iter().find(|s| s.is_leased_by(workspace, name)) {
            sim.acquirer = Acquirer::User;
            self.save_sim(&sim).await?;
        }
        Ok(())
    }

    pub async fn remove_simulators(&self, owner: &SpaceId) -> Result<()> {
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
        let idle_seconds = self.config.current().machine.simulators.idle_seconds;
        for mut sim in self.list_simulators(None).await? {
            if sim.workspace_id.is_none()
                && UnixTime::now().duration_since(sim.last_used)
                    >= std::time::Duration::from_secs(idle_seconds)
            {
                self.delete_sim(&mut sim).await?;
            }
        }
        Ok(())
    }
}

/// What a release of a simulator lease did.
pub(crate) enum SimRelease {
    /// Nothing: the lease is no longer the holder's.
    Kept,
    /// It released the lease, with the device it left idle; none when the
    /// lease had no device yet, which it deleted.
    Released(Option<Box<Simulator>>),
}

/// What a release does with the device it leaves idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Device {
    /// Keep it as it is, booted for the next lease.
    Keep,
    /// Shut it down, keeping its apps and data, to free what it runs.
    ShutDown,
    /// Keep it for the request that holds the simulators' lock and
    /// allocates next, which may take this device.
    ForRequest,
}

/// The simctl device name Shoal gives its own devices.
fn device_name(sim: &Simulator) -> String {
    format!("shoal-{}", sim.id)
}
