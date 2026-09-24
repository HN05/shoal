//! Deterministic simulator decisions over snapshots; no persistence or simctl calls.
use anyhow::{Result, bail, ensure};

use super::{Profile, SimConfig, SimRequest, Simulator, SimulatorState, simctl::Inventory};
use crate::config::Simulators;

/// A proposal over recorded devices, valid only while the simulator gate is held.
/// The executor rechecks ownership and live capacity before applying it.
pub(super) struct Plan<'a> {
    pub reusable: Option<&'a Simulator>,
    pub idle: Vec<&'a Simulator>,
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
    let mut idle: Vec<_> = records
        .iter()
        .filter(|s| s.workspace_id.is_none() && reusable.as_ref().is_none_or(|r| r.id != s.id))
        .copied()
        .collect();
    if clean {
        idle.sort_by_key(|s| (s.app_cost(), s.last_used));
    } else {
        idle.sort_by_key(|s| s.last_used);
    }

    let needed = if reusable.is_none() && records.len() >= limits.max_devices {
        records.len() - limits.max_devices + 1
    } else {
        0
    };
    let evictions: Vec<_> = idle.iter().copied().take(needed).collect();
    let has_device_capacity = evictions.len() == needed;
    Plan {
        reusable,
        idle,
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
pub(super) fn check_idle_record(expected: &Simulator, records: &[Simulator]) -> Result<()> {
    ensure!(
        records.iter().any(|s| s.id == expected.id
            && s.udid == expected.udid
            && s.workspace_id.is_none()
            && s.state == expected.state
            && s.device == expected.device
            && s.runtime == expected.runtime),
        "planned simulator is no longer recorded and idle: {}",
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
) -> Result<Profile> {
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
            requires_approval: false,
            approval_lifetime: crate::daemon::access::Lifetime::Lease,
            device: device.clone(),
            runtime: runtime.clone(),
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
    profile.approval_lifetime = if lifetimes.contains(&crate::daemon::access::Lifetime::Lease) {
        crate::daemon::access::Lifetime::Lease
    } else {
        crate::daemon::access::Lifetime::Workspace
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
        ..profile.clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{daemon::access::Lifetime, sim::simctl::DeviceState};
    use serde_json::json;

    fn inventory() -> Inventory {
        serde_json::from_value(json!({
            "devicetypes": [{"identifier": "phone", "name": "Phone"}],
            "runtimes": [{"identifier": "ios", "name": "iOS", "isAvailable": true}],
            "devices": {"ios": []}
        }))
        .unwrap()
    }

    fn profile() -> Profile {
        Profile {
            device: "phone".into(),
            runtime: "ios".into(),
            requires_approval: false,
            approval_lifetime: Lifetime::Lease,
        }
    }

    fn request() -> SimRequest {
        SimRequest {
            request_id: "request".into(),
            clean: false,
            name: "default".into(),
            profile: None,
            device: None,
            runtime: None,
            reason: None,
        }
    }

    fn record(id: &str, apps: Option<usize>, last_used: u64) -> Simulator {
        Simulator {
            id: id.into(),
            udid: Some(id.into()),
            device: "phone".into(),
            runtime: "ios".into(),
            workspace_id: None,
            last_workspace_id: Some("previous".into()),
            lease_name: None,
            reason: None,
            state: SimulatorState::Idle,
            last_used,
            error: None,
            installed_apps: apps,
        }
    }

    fn add_device(inventory: &mut Inventory, sim: &Simulator, state: DeviceState) {
        inventory
            .devices
            .get_mut("ios")
            .unwrap()
            .push(super::super::simctl::Device {
                udid: sim.udid.clone().unwrap(),
                name: super::super::device_name(sim),
                state,
                is_available: true,
            });
    }

    fn planned<'a>(records: &'a [Simulator], clean: bool, max_devices: usize) -> Plan<'a> {
        let mut inventory = inventory();
        for sim in records {
            add_device(&mut inventory, sim, DeviceState::Booted);
        }
        plan(
            records,
            &inventory,
            &SimConfig {
                max_devices,
                ..Default::default()
            },
            &profile(),
            clean,
        )
    }

    #[test]
    fn normal_handoff_reuses_most_recent_compatible_idle_device() {
        let mut records = vec![record("old", Some(0), 1), record("recent", Some(8), 2)];
        let selected = planned(&records, false, 2);
        assert_eq!(selected.reusable.unwrap().id, "recent");
        assert!(selected.evictions.is_empty());
        records[1].workspace_id = Some("other".into());
        assert_eq!(planned(&records, false, 2).reusable.unwrap().id, "old");
        records[0].runtime = "other".into();
        let selected = planned(&records, false, 2);
        assert!(selected.reusable.is_none());
        assert_eq!(selected.evictions[0].id, "old");
        let mut inventory = inventory();
        add_device(&mut inventory, &records[0], DeviceState::Booted);
        records[0].runtime = "ios".into();
        inventory.devices.get_mut("ios").unwrap()[0].is_available = false;
        assert!(
            plan(
                &records,
                &inventory,
                &SimConfig::default(),
                &profile(),
                false
            )
            .reusable
            .is_none()
        );
    }

    #[test]
    fn clean_allocation_uses_spare_capacity_then_lowest_app_cost() {
        let records = vec![record("useful", Some(5), 1), record("cheap", Some(1), 2)];
        let spare = planned(&records, true, 3);
        assert!(spare.reusable.is_none());
        assert!(spare.evictions.is_empty());
        let full = planned(&records, true, 2);
        assert_eq!(full.reusable.unwrap().id, "cheap");
        assert!(full.evictions.is_empty());
        let mut records = records;
        records[1].device = "tablet".into();
        let cheaper_eviction = planned(&records, true, 2);
        assert!(cheaper_eviction.reusable.is_none());
        assert_eq!(cheaper_eviction.evictions[0].id, "cheap");
        records[1].installed_apps = Some(5);
        assert_eq!(planned(&records, true, 2).reusable.unwrap().id, "useful");
        records[1].installed_apps = None;
        assert_eq!(planned(&records, true, 2).reusable.unwrap().id, "useful");
    }

    #[test]
    fn eviction_sums_costs_when_limits_shrink_and_unknown_costs_sort_last() {
        let mut records = vec![
            record("useful", Some(5), 4),
            record("empty", Some(0), 1),
            record("small", Some(2), 2),
            record("unknown", None, 3),
        ];
        for sim in &mut records[1..] {
            sim.device = "tablet".into();
        }
        let selected = planned(&records, true, 3);
        assert!(selected.reusable.is_none());
        assert_eq!(
            selected
                .evictions
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            ["empty", "small"]
        );
        records[2].installed_apps = Some(6);
        assert_eq!(planned(&records, true, 3).reusable.unwrap().id, "useful");
        // With no compatible reuse, unknown cost follows all known counts.
        records[0].device = "tablet".into();
        let selected = planned(&records, true, 1);
        assert_eq!(
            selected
                .evictions
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            ["empty", "useful", "small", "unknown"]
        );
    }

    #[test]
    fn full_capacity_never_selects_owned_devices_for_eviction() {
        let mut records = vec![record("owned", Some(0), 1), record("idle", Some(3), 2)];
        records[0].workspace_id = Some("owner".into());
        records[1].device = "tablet".into();
        let selected = planned(&records, true, 1);
        assert!(!selected.has_device_capacity);
        assert_eq!(selected.evictions.len(), 1);
        assert_eq!(selected.evictions[0].id, "idle");
        records[1].workspace_id = Some("other".into());
        let selected = planned(&records, false, 2);
        assert!(!selected.has_device_capacity);
        assert!(selected.idle.is_empty());
        assert!(selected.evictions.is_empty());
    }

    #[test]
    fn running_capacity_uses_live_native_states_including_external_devices() {
        let mut inventory = inventory();
        let sim = record("reusable", Some(0), 1);
        add_device(&mut inventory, &sim, DeviceState::Booted);
        assert!(has_running_capacity(&inventory, 1, Some(&sim)));
        assert!(!has_running_capacity(&inventory, 1, None));
        add_device(
            &mut inventory,
            &record("external", None, 0),
            DeviceState::Unknown("future".into()),
        );
        assert!(!has_running_capacity(&inventory, 1, Some(&sim)));
        inventory.devices.get_mut("ios").unwrap()[1].state = DeviceState::Shutdown;
        assert!(has_running_capacity(&inventory, 1, Some(&sim)));
        inventory.devices.get_mut("ios").unwrap()[0].state = DeviceState::ShuttingDown;
        assert!(!has_running_capacity(&inventory, 1, Some(&sim)));
        inventory.devices.get_mut("ios").unwrap()[0].state = DeviceState::Shutdown;
        assert!(has_running_capacity(&inventory, 1, None));
    }

    #[test]
    fn plan_targets_require_current_unowned_records_with_matching_identity() {
        let sim = record("idle", Some(1), 1);
        assert!(check_idle_record(&sim, &[]).is_err());
        assert!(check_idle_record(&sim, std::slice::from_ref(&sim)).is_ok());
        let mut changed = sim.clone();
        changed.workspace_id = Some("new-owner".into());
        assert!(check_idle_record(&sim, &[changed]).is_err());
        let mut changed = sim.clone();
        changed.udid = Some("external".into());
        assert!(check_idle_record(&sim, &[changed]).is_err());
    }

    #[test]
    fn existing_lease_checks_settings_and_confirmed_native_state() {
        let mut inventory = inventory();
        let mut sim = record("owned", Some(4), 1);
        sim.workspace_id = Some("owner".into());
        sim.lease_name = Some("default".into());
        sim.state = SimulatorState::Leased;
        add_device(&mut inventory, &sim, DeviceState::Booted);
        assert!(check_existing_request(&sim, &request(), None).is_ok());
        assert!(check_existing_request(&sim, &request(), Some(&profile())).is_ok());
        assert!(check_existing_device(&sim, &inventory).is_ok());
        let mut clean = request();
        clean.clean = true;
        assert!(check_existing_request(&sim, &clean, None).is_err());
        let mut different = profile();
        different.runtime = "other".into();
        assert!(check_existing_request(&sim, &request(), Some(&different)).is_err());
        for state in [
            DeviceState::Shutdown,
            DeviceState::Booting,
            DeviceState::Unknown("booted".into()),
        ] {
            inventory.devices.get_mut("ios").unwrap()[0].state = state;
            assert!(check_existing_device(&sim, &inventory).is_err());
        }
        inventory.devices.get_mut("ios").unwrap()[0].state = DeviceState::Booted;
        inventory.devices.get_mut("ios").unwrap()[0].is_available = false;
        assert!(check_existing_device(&sim, &inventory).is_err());
        inventory.devices.get_mut("ios").unwrap()[0].is_available = true;
        sim.state = SimulatorState::Failed;
        assert!(check_existing_device(&sim, &inventory).is_err());
    }

    #[test]
    fn profile_resolution_rejects_unavailable_runtime_and_preserves_approval() {
        let mut inventory = inventory();
        let mut config = SimConfig {
            default: Some("phone".into()),
            ..Default::default()
        };
        config.profiles.insert(
            "phone".into(),
            Profile {
                requires_approval: true,
                approval_lifetime: Lifetime::Workspace,
                ..profile()
            },
        );
        let mut repo = Simulators {
            requires_approval: false,
            ..Default::default()
        };
        let resolved = resolve_request(&config, &repo, &request(), &inventory).unwrap();
        assert!(resolved.requires_approval);
        assert_eq!(resolved.approval_lifetime, Lifetime::Workspace);
        repo.requires_approval = true;
        repo.approval_lifetime = Lifetime::Lease;
        let mut explicit = request();
        explicit.device = Some("Phone".into());
        explicit.runtime = Some("iOS".into());
        let resolved = resolve_request(&config, &repo, &explicit, &inventory).unwrap();
        assert_eq!(resolved.device, "phone");
        assert!(resolved.requires_approval);
        assert_eq!(resolved.approval_lifetime, Lifetime::Lease);
        inventory.runtimes[0].is_available = false;
        let error = resolve_request(&config, &repo, &request(), &inventory).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("runtime is not installed/available")
        );
    }
}
