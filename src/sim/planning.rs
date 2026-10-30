//! Deterministic simulator decisions over snapshots; no persistence or simctl calls.
use anyhow::{Result, bail, ensure};

use super::{Profile, SimConfig, SimRequest, Simulator, SimulatorState, simctl::Inventory};
use crate::config::Simulators;

/// A proposal over recorded devices, valid only while the simulator gate is held.
/// The executor rechecks ownership and live capacity before applying it.
pub(super) struct Plan<'a> {
    pub reusable: Option<&'a Simulator>,
    /// Unowned records eligible for shutdown or eviction, excluding reuse.
    pub unowned_candidates: Vec<&'a Simulator>,
    pub evictions: Vec<&'a Simulator>,
    pub has_device_capacity: bool,
}

pub(super) fn plan<'a>(
    records: &'a [Simulator],
    inventory: &Inventory,
    limits: &SimConfig,
    profile: &Profile,
    clean: bool,
) -> Plan<'a> {
    let mut records: Vec<_> = records.iter().collect();
    if clean && records.len() >= limits.max_devices {
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
                && s.device(inventory).is_some_and(|d| d.is_available)
        })
        .copied();
    if clean {
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
    let mut unowned_candidates: Vec<_> = records
        .iter()
        .filter(|s| s.workspace_id.is_none() && reusable.as_ref().is_none_or(|r| r.id != s.id))
        .copied()
        .collect();
    if clean {
        unowned_candidates.sort_by_key(|s| (s.app_cost(), s.last_used));
    } else {
        unowned_candidates.sort_by_key(|s| s.last_used);
    }

    let needed = if reusable.is_none() && records.len() >= limits.max_devices {
        records.len() - limits.max_devices + 1
    } else {
        0
    };
    let evictions: Vec<_> = unowned_candidates.iter().copied().take(needed).collect();
    let has_device_capacity = evictions.len() == needed;
    Plan {
        reusable,
        unowned_candidates,
        evictions,
        has_device_capacity,
    }
}

/// Count external and transitional devices too; only a confirmed booted reuse
/// can occupy the last slot without requiring another one.
pub(super) fn has_running_capacity(
    inventory: &Inventory,
    max_booted: usize,
    reusable: Option<&Simulator>,
) -> bool {
    inventory.running_count() < max_booted
        || (inventory.running_count() == max_booted
            && reusable.is_some_and(|s| s.is_booted(inventory)))
}

/// A plan is not ownership evidence. Match its targets against current records.
pub(super) fn check_unowned_record(expected: &Simulator, records: &[Simulator]) -> Result<()> {
    ensure!(
        records.iter().any(|s| s.id == expected.id
            && s.udid == expected.udid
            && s.workspace_id.is_none()
            && s.state == expected.state
            && s.device == expected.device
            && s.runtime == expected.runtime),
        "planned simulator record changed or is now owned: {}",
        expected.id
    );
    Ok(())
}

pub(super) fn check_existing_request(
    sim: &Simulator,
    request: &SimRequest,
    profile: Option<&Profile>,
) -> Result<()> {
    ensure!(
        !request.clean,
        "this simulator name is already leased; release it before requesting a clean device, or use another --name"
    );
    if let Some(profile) = profile {
        ensure!(
            sim.device == profile.device && sim.runtime == profile.runtime,
            "simulator name already leased with different settings; release it first"
        );
    }
    Ok(())
}

pub(super) fn check_existing_device(sim: &Simulator, inventory: &Inventory) -> Result<()> {
    ensure!(
        sim.state == SimulatorState::Leased,
        "simulator allocation is {}; release it to clean up and retry",
        sim.state
    );
    ensure!(
        sim.device(inventory)
            .is_some_and(|d| d.is_available && d.state.is_booted()),
        "leased simulator is no longer booted/available; release it and acquire again"
    );
    Ok(())
}

pub(super) fn resolve_request(
    config: &SimConfig,
    repo: &Simulators,
    request: &SimRequest,
    inventory: &Inventory,
    existing_runtime: Option<&str>,
) -> Result<Profile> {
    ensure!(
        request.profile.is_none() || (request.device.is_none() && request.runtime.is_none()),
        "use either --profile or --device with optional --runtime"
    );
    let resolve = |profile: &Profile| resolve_profile(inventory, profile, existing_runtime);
    let profile = if request.device.is_some() || request.runtime.is_some() {
        let Some(device) = &request.device else {
            bail!("--runtime requires --device");
        };
        Profile {
            requires_approval: false,
            approval_lifetime: crate::daemon::approvals::Lifetime::Lease,
            device: device.clone(),
            runtime: request.runtime.clone().unwrap_or_default(),
        }
    } else {
        let names = if let Some(name) = &request.profile {
            vec![name.clone()]
        } else if !repo.preferred.is_empty() {
            repo.preferred.clone()
        } else {
            config.default.iter().cloned().collect()
        };
        ensure!(
            !names.is_empty(),
            "select --profile or configure machine.simulators.default and machine.simulators.profiles in global config"
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
    let mut profile = resolve(&profile)?;
    let matching: Vec<_> = config
        .profiles
        .values()
        .filter_map(|p| resolve(p).ok())
        .filter(|p| p.device == profile.device && p.runtime == profile.runtime)
        .collect();
    let allowed = !matching.is_empty();
    let mut lifetimes: Vec<_> = matching
        .iter()
        .filter(|p| p.requires_approval)
        .map(|p| p.approval_lifetime)
        .collect();
    if repo.requires_approval {
        lifetimes.push(repo.approval_lifetime);
    }
    profile.requires_approval = !lifetimes.is_empty();
    profile.approval_lifetime = if lifetimes.contains(&crate::daemon::approvals::Lifetime::Lease) {
        crate::daemon::approvals::Lifetime::Lease
    } else {
        crate::daemon::approvals::Lifetime::Workspace
    };
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

/// Match a profile's device/runtime names or identifiers against what is
/// installed, returning canonical identifiers.
fn resolve_profile(
    inventory: &Inventory,
    profile: &Profile,
    existing_runtime: Option<&str>,
) -> Result<Profile> {
    let devices: Vec<_> = inventory
        .devicetypes
        .iter()
        .filter(|d| d.identifier == profile.device || d.name == profile.device)
        .collect();
    ensure!(
        devices.len() == 1,
        "device type is unavailable or ambiguous: {}",
        profile.device
    );
    let requested = if profile.runtime.is_empty() {
        existing_runtime.unwrap_or_default()
    } else {
        &profile.runtime
    };
    let runtime = resolve_runtime(inventory, devices[0], requested)?;
    Ok(Profile {
        device: devices[0].identifier.clone(),
        runtime: runtime.identifier.clone(),
        ..profile.clone()
    })
}

fn resolve_runtime<'a>(
    inventory: &'a Inventory,
    device: &super::simctl::DeviceType,
    requested: &str,
) -> Result<&'a super::simctl::Runtime> {
    if requested.is_empty() {
        return inventory
            .runtimes
            .iter()
            .filter(|r| r.is_available && r.supports(device))
            .filter_map(|r| r.ios_version().map(|version| (version, r)))
            .max_by(|(a, ar), (b, br)| a.cmp(b).then_with(|| ar.identifier.cmp(&br.identifier)))
            .map(|(_, runtime)| runtime)
            .ok_or_else(|| anyhow::anyhow!(
                "no installed/available iOS runtime supports device type {}; Shoal does not download runtimes",
                device.name
            ));
    }
    let runtimes: Vec<_> = inventory
        .runtimes
        .iter()
        .filter(|r| r.is_available && (r.identifier == requested || r.name == requested))
        .collect();
    ensure!(
        runtimes.len() == 1,
        "runtime is not installed/available or is ambiguous: {}; Shoal does not download runtimes",
        requested
    );
    ensure!(
        runtimes[0].supports(device),
        "device type {} is incompatible with runtime {}",
        device.name,
        requested
    );
    Ok(runtimes[0])
}

#[cfg(test)]
mod tests;
