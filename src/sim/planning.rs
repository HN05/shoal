//! Deterministic simulator decisions over snapshots; no persistence or simctl calls.
use anyhow::{Result, bail, ensure};

use super::{Profile, SimConfig, SimRequest, Simulator, SimulatorState, simctl::Inventory};
use crate::config::repo::RepoConfig;

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
    repo: &RepoConfig,
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
        } else if !repo.simulators.preferred.is_empty() {
            repo.simulators.preferred.clone()
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
    if repo
        .simulators
        .requires_approval
        .unwrap_or(config.requires_approval)
    {
        lifetimes.push(
            repo.simulators
                .approval_lifetime
                .unwrap_or(config.approval_lifetime),
        );
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
        let mut repo = RepoConfig::default();
        repo.simulators.requires_approval = Some(false);
        let resolved = resolve_request(&config, &repo, &request(), &inventory).unwrap();
        assert!(resolved.requires_approval);
        assert_eq!(resolved.approval_lifetime, Lifetime::Workspace);
        repo.simulators.requires_approval = Some(true);
        repo.simulators.approval_lifetime = Some(Lifetime::Lease);
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
