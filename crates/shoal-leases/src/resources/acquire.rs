//! Acquiring a resource lease: claiming it in one transaction, renewing one
//! held under the same name, and freeing capacity asleep agents hold.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use shoal_ids::space_ids::SpaceId;
use std::collections::BTreeSet;
use uuid::Uuid;

use super::{
    freeing,
    records::{
        ResourceLeaseExt, insert_lease, named_lease, pool_leases, save_definition,
        stored_definition,
    },
    repositories,
    usage::{PoolUsage, ResourceConfigExt, select_member},
};
use crate::{
    Leases,
    acquired_leases::Acquired,
    allocation::Allocation,
    callers::Caller,
    daemon::{Daemon, Records},
    gates::{GuardMode, ResourceGuard},
    hooks::{Hook, HookKind},
    model::Workspace,
    queue,
    sim::Device,
    store, validate,
};
use shoal_daemon_base::approval_requests::ApprovalRequestExt;
use shoal_protocol::acquirers::Acquirer;
use shoal_protocol::approvals::{ApprovalRequest, ResourceSpecification, Specification, Target};
pub use shoal_protocol::resources::{Definition, ResourceLease, ResourceRequest, Scope};
use shoal_protocol::space_names::SpaceSelector;

impl Leases {
    pub async fn acquire_resource(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        request: ResourceRequest,
        caller: Option<&Caller>,
    ) -> Result<Allocation<ResourceLease>> {
        validate::lowercase_name("resource", &request.pool)?;
        validate::lowercase_name("resource", &request.name)?;
        if let Some(resource) = &request.resource {
            validate::lowercase_name("resource", resource)?;
        }
        validate::reason("resource", request.reason.as_deref())?;
        let workspace = daemon.workspace(selector).await?;
        daemon.touch(&workspace.id).await;
        let mut definitions = self.resource_definitions(daemon, &workspace).await?;
        let (scope, definition) = definitions
            .remove(&request.pool)
            .ok_or_else(|| anyhow::anyhow!("unknown resource or pool: {}", request.pool))?;
        let hook = daemon
            .hook(&workspace, HookKind::PostResourceAcquire)
            .await?;
        let target = Target::Resource {
            scope: scope.clone(),
            pool: request.pool.clone(),
        };
        let mut place = self
            .queues
            .enter(target, &workspace.id, &request.name, request.queue);
        let asked = ResourceClaim {
            workspace: &workspace,
            scope: &scope,
            definition: &definition,
            request: &request,
            caller,
            hooked: hook.is_some(),
        };
        let mut claimed = self.claim_resource(daemon, &asked, place.ahead()).await;
        // Capacity asleep agents hold goes to the request, which claims again.
        if let Ok(claim) = &mut claimed
            && !claim.freeing.is_empty()
        {
            let freeing = permits(std::mem::take(&mut claim.freeing));
            // Their releases take their workspaces' guards, this one's too.
            claim.guard = None;
            let given = self.give_to_request(daemon, &workspace, caller, freeing, Device::Keep);
            if given.await {
                claimed = self.claim_resource(daemon, &asked, place.ahead()).await;
            }
        }
        // The guard stays until the hook that follows the claim has run.
        let (acquisition, _resources) = match claimed {
            Ok(claim) => (Ok(claim.allocation), claim.guard),
            Err(error) => (Err(error), None),
        };
        place.settle(&acquisition);
        let acquisition = acquisition?;
        if let Allocation::Granted(lease) = &acquisition
            && let Some(command) = hook
        {
            (daemon.run_hook(&workspace, Hook::PostResourceAcquire(lease), &command))
                .await
                .context(
                    "resource lease retained; repeat acquire to retry the hook, or release it",
                )?;
        }
        let space = (&workspace.name, &workspace.id);
        (self.notify_allocation(daemon, space, &acquisition, "")).await;
        Ok(acquisition)
    }

    /// Claim the lease `claim` asks for in one transaction, with `ahead`
    /// queued requests that arrived earlier, under the workspace's resource
    /// guard. The guard is exclusive when a hook runs after the claim, and
    /// the claim keeps it. A hook in the workspace holds the guard briefly,
    /// so a queued request waits it out in place instead of failing.
    async fn claim_resource(
        &self,
        daemon: &dyn Daemon,
        claim: &ResourceClaim<'_>,
        ahead: usize,
    ) -> Result<Claim> {
        let workspace = claim.workspace;
        let related = repositories::related(daemon, claim.definition).await?;
        let mode = match claim.hooked {
            true => GuardMode::Exclusive,
            false => GuardMode::Shared,
        };
        let guard = match self.gates.resource_guard(&workspace.id, mode) {
            Ok(guard) => guard,
            Err(error) if claim.request.queue => {
                return Ok(Allocation::Busy(error.to_string()).into());
            }
            Err(error) => return Err(error),
        };
        if claim.hooked {
            daemon.verify_worktree(workspace).await?;
        }
        let claimant = Claimant {
            workspace_id: workspace.id.clone(),
            caller: claim.caller.cloned(),
            acquirer: self.acquirer(daemon, claim.caller).await,
            ahead,
        };
        let (scope, definition) = (claim.scope.clone(), claim.definition.clone());
        let request = claim.request.clone();
        let records = self.records.clone();
        let claimed = self.store.run(move |db| {
            acquire_lease(db, &*records, claimant, scope, definition, request, related)
        });
        Ok(Claim {
            guard: Some(guard),
            ..claimed.await?
        })
    }
}

/// `; held by a, b` naming the workspaces with leases in the pool, for the
/// busy message and the notification the user sees.
pub(super) fn holders(
    db: &Connection,
    records: &dyn Records,
    active: &[ResourceLease],
) -> Result<String> {
    let ids: BTreeSet<&SpaceId> = active.iter().map(|l| &l.workspace_id).collect();
    let mut names = Vec::with_capacity(ids.len());
    for id in ids {
        let name = records.space_name(db, id)?;
        names.push(name.map_or_else(|| id.to_string(), |name| name.to_string()));
    }
    Ok(if names.is_empty() {
        String::new()
    } else {
        format!("; held by {}", names.join(", "))
    })
}

/// Each set of `leases` as the leases agents acquire.
pub(super) fn permits(leases: Vec<Vec<ResourceLease>>) -> Vec<Vec<Acquired>> {
    let permits = |set: Vec<ResourceLease>| set.into_iter().map(Acquired::permit).collect();
    leases.into_iter().map(permits).collect()
}

/// What a request for a resource lease asks for.
pub(super) struct ResourceClaim<'a> {
    workspace: &'a Workspace,
    scope: &'a Scope,
    definition: &'a Definition,
    request: &'a ResourceRequest,
    caller: Option<&'a Caller>,
    /// Whether a hook runs once the lease is claimed.
    hooked: bool,
}

/// What a claim came to, with the sets of held leases any one of which
/// would grant it once released, where capacity keeps it busy, and the
/// workspace's resource guard it was made under.
pub(super) struct Claim {
    allocation: Allocation<ResourceLease>,
    freeing: Vec<Vec<ResourceLease>>,
    guard: Option<ResourceGuard>,
}

impl From<Allocation<ResourceLease>> for Claim {
    fn from(allocation: Allocation<ResourceLease>) -> Self {
        Self {
            allocation,
            freeing: Vec::new(),
            guard: None,
        }
    }
}

/// The workspace asking for a lease and its standing.
pub(super) struct Claimant {
    workspace_id: SpaceId,
    /// Who asks for it, which a scoped caller does with configured approvals.
    caller: Option<Caller>,
    /// Whose lease it becomes, which decides how long it lasts.
    acquirer: Acquirer,
    /// Queued requests that arrived earlier.
    ahead: usize,
}

/// Claim or renew atomically. Approval and grant outcomes commit; busy outcomes
/// leave the pool definition and access records unchanged, so this opens its
/// own transaction instead of running in `Store::write`. Earlier queued
/// requests keep new claims busy but never block renewal or approval.
pub(super) fn acquire_lease(
    db: &mut Connection,
    records: &dyn Records,
    claimant: Claimant,
    scope: Scope,
    definition: Definition,
    mut request: ResourceRequest,
    mut related: repositories::Related,
) -> Result<Claim> {
    let Claimant {
        workspace_id,
        caller,
        acquirer,
        ahead,
    } = claimant;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    store::require_ready(&tx, &workspace_id)?;
    if let Some(existing) = named_lease(&tx, &workspace_id, &request.pool, &request.name)? {
        return renew_lease(tx, &existing, &scope, request, &acquirer).map(Claim::from);
    }
    if let Some(resource) = &request.resource {
        ensure!(
            definition.resources.contains_key(resource),
            "unknown resource {resource} in pool {}",
            request.pool
        );
    }
    let active = pool_leases(&tx, &scope, &request.pool)?;
    if let Some(stored) = stored_definition(&tx, &scope, &request.pool)? {
        ensure!(
            active.is_empty() || stored == definition,
            "resource definition changed while leases are active; align repo configs or release all leases before changing kind/capacity/members"
        );
    }
    save_definition(&tx, &scope, &request.pool, &definition)?;
    let target = Target::Resource {
        scope: scope.clone(),
        pool: request.pool.clone(),
    };
    if caller.is_some()
        && let Some(pending) =
            records.current_approval(&tx, &workspace_id, &target, &request.name)?
    {
        let Specification::Resource(bound) = &pending.specification else {
            bail!(
                "approval request {} for {target} does not select a pool member; release the name and request again",
                pending.number
            );
        };
        ensure!(
            request
                .resource
                .as_deref()
                .is_none_or(|r| r == bound.member),
            "approval request member changed; release it first"
        );
        request.resource = Some(bound.member.clone());
    }
    let usage: PoolUsage<'_> = active.iter().collect();
    let pool_available = definition.capacity.saturating_sub(usage.slots);
    let selected = match select_member(&definition, &request, &usage, pool_available)? {
        Some(selected) => Some(selected),
        // A busy pool still selects the member an approval request binds.
        None => select_member(
            &definition,
            &request,
            &PoolUsage::default(),
            definition.capacity,
        )?,
    };
    let busy = || -> Result<Allocation<ResourceLease>> {
        Ok(Allocation::Busy(format!(
            "no compatible capacity for {} in pool {}{}",
            request.resource.as_deref().unwrap_or("any resource"),
            request.pool,
            holders(&tx, records, &active)?
        )))
    };
    let Some((resource, settings, mode)) = selected else {
        return busy().map(Claim::from);
    };
    let repository = settings
        .repo
        .as_deref()
        .map(|selector| repositories::resolve(&tx, records, selector, &mut related))
        .transpose()?;
    if caller.is_some() && settings.requires_approval {
        let bound = ResourceSpecification {
            repository: repository.clone(),
            definition: definition.clone(),
            member: resource.clone(),
            mode,
        };
        let approval = ApprovalRequest::new(
            &workspace_id,
            target,
            &request.name,
            Specification::Resource(bound),
            settings.approval_lifetime,
            request.reason.as_deref(),
        );
        if let Some(approval) = records.check_approval(&tx, approval)? {
            tx.commit()?;
            return Ok(Allocation::Approval(Box::new(approval)).into());
        }
    }
    if let Some(queued) = queue::behind(ahead, &format!("pool {}", request.pool)) {
        return Ok(queued.into());
    }
    if !settings.can_acquire(mode, usage.member(resource), pool_available) {
        return Ok(Claim {
            allocation: busy()?,
            freeing: freeing::freeing(&definition, &request, &active),
            guard: None,
        });
    }
    let lease = ResourceLease {
        repository,
        mode,
        id: Uuid::new_v4().to_string(),
        workspace_id,
        scope,
        pool: request.pool,
        name: request.name,
        resource: resource.clone(),
        reason: request
            .reason
            .or_else(|| settings.reason.clone())
            .or(definition.reason.clone()),
        created_at: crate::time::UnixTime::now(),
        acquirer,
    };
    insert_lease(&tx, &lease)?;
    repositories::record(&tx, &lease)?;
    tx.commit()?;
    Ok(Allocation::Granted(lease).into())
}

/// An existing lease with the same name is returned again, refreshing its
/// reason and, for the user's request, becoming the user's, as long as the
/// request does not contradict it.
pub(super) fn renew_lease(
    tx: Transaction<'_>,
    existing: &ResourceLease,
    scope: &Scope,
    request: ResourceRequest,
    acquirer: &Acquirer,
) -> Result<Allocation<ResourceLease>> {
    ensure!(
        existing.scope == *scope
            && request.mode.is_none_or(|m| m == existing.mode)
            && request
                .resource
                .as_ref()
                .is_none_or(|r| r == &existing.resource)
            && request.kind.is_none_or(|kind| kind == existing.kind()),
        "lease name already acquired with different settings; release it first"
    );
    let mut lease = existing.clone();
    if let Some(reason) = request.reason {
        tx.execute(
            "UPDATE resource_leases SET reason=?2 WHERE id=?1",
            params![lease.id, reason],
        )?;
        lease.reason = Some(reason);
    }
    let renewed = lease.acquirer.renewed_by(acquirer);
    if renewed != lease.acquirer {
        tx.execute(
            "UPDATE resource_leases SET acquirer=?2,acquirer_id=?3 WHERE id=?1",
            params![lease.id, renewed.kind(), renewed.id()],
        )?;
        lease.acquirer = renewed;
    }
    tx.commit()?;
    Ok(Allocation::Granted(lease))
}
