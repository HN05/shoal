use super::*;
use crate::{daemon::approvals::Lifetime, sim::simctl::DeviceState};
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
        queue: false,
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
    assert!(selected.unowned_candidates.is_empty());
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
    assert!(check_unowned_record(&sim, &[]).is_err());
    assert!(check_unowned_record(&sim, std::slice::from_ref(&sim)).is_ok());
    let mut changed = sim.clone();
    changed.workspace_id = Some("new-owner".into());
    assert!(check_unowned_record(&sim, &[changed]).is_err());
    let mut changed = sim.clone();
    changed.udid = Some("external".into());
    assert!(check_unowned_record(&sim, &[changed]).is_err());
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
fn omitted_runtime_selects_latest_compatible_available_ios_and_keeps_approval() {
    let mut inventory = inventory();
    inventory.runtimes = serde_json::from_value(json!([
            {"identifier": "com.apple.CoreSimulator.SimRuntime.iOS-26-9", "name": "iOS 26.9", "version": "26.9", "isAvailable": true},
            {"identifier": "com.apple.CoreSimulator.SimRuntime.iOS-9-3", "name": "iOS 9.3", "version": "9.3", "isAvailable": true},
            {"identifier": "com.apple.CoreSimulator.SimRuntime.iOS-26-10-1", "name": "iOS 26.10.1", "version": "26.10.1", "isAvailable": true},
            {"identifier": "com.apple.CoreSimulator.SimRuntime.iOS-26-10", "name": "iOS 26.10", "version": "26.10", "isAvailable": true},
            {"identifier": "com.apple.CoreSimulator.SimRuntime.iOS-27-0", "name": "iOS 27", "version": "27.0", "isAvailable": false},
            {"identifier": "com.apple.CoreSimulator.SimRuntime.iOS-28-0", "name": "iOS 28", "version": "28.0", "isAvailable": true, "supportedDeviceTypes": [{"identifier": "tablet", "name": "Tablet"}]},
            {"identifier": "com.apple.CoreSimulator.SimRuntime.tvOS-29-0", "name": "tvOS 29", "version": "29.0", "isAvailable": true},
            {"identifier": "com.apple.CoreSimulator.SimRuntime.iOS-Unknown", "name": "iOS Unknown", "version": "invalid", "isAvailable": true}
        ])).unwrap();
    let mut config = SimConfig::default();
    let unpinned: Profile = toml::from_str(
        "device = 'Phone'\nrequires_approval = true\napproval_lifetime = 'workspace'",
    )
    .unwrap();
    assert!(unpinned.runtime.is_empty());
    config.profiles.insert("phone".into(), unpinned);
    config.default = Some("phone".into());
    config.validate().unwrap();
    let repo = Simulators::default();
    let newest = "com.apple.CoreSimulator.SimRuntime.iOS-26-10-1";
    let mut direct = request();
    direct.device = Some("Phone".into());
    for request in [request(), direct] {
        let resolved = resolve_request(&config, &repo, &request, &inventory, None).unwrap();
        assert_eq!(resolved.runtime, newest);
        assert!(resolved.requires_approval);
        assert_eq!(resolved.approval_lifetime, Lifetime::Workspace);
        let retained = resolve_request(
            &config,
            &repo,
            &request,
            &inventory,
            Some("com.apple.CoreSimulator.SimRuntime.iOS-26-9"),
        )
        .unwrap();
        assert_eq!(
            retained.runtime,
            "com.apple.CoreSimulator.SimRuntime.iOS-26-9"
        );
        assert!(retained.requires_approval);
    }
    config.allow_any = true;
    let mut explicit = request();
    explicit.device = Some("Phone".into());
    explicit.runtime = Some("iOS 26.9".into());
    explicit.reason = Some("pinned runtime".into());
    assert_eq!(
        resolve_request(&config, &repo, &explicit, &inventory, None)
            .unwrap()
            .runtime,
        "com.apple.CoreSimulator.SimRuntime.iOS-26-9"
    );
    explicit.runtime = Some("iOS 27".into());
    assert!(resolve_request(&config, &repo, &explicit, &inventory, None).is_err());
    explicit.runtime = Some("iOS 28".into());
    assert!(resolve_request(&config, &repo, &explicit, &inventory, None).is_err());
    for runtime in &mut inventory.runtimes {
        if runtime.supports(&inventory.devicetypes[0]) && runtime.ios_version().is_some() {
            runtime.is_available = false;
        }
    }
    assert!(
        resolve_request(&config, &repo, &request(), &inventory, None)
            .unwrap_err()
            .to_string()
            .contains("no installed/available iOS runtime supports")
    );
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
    let resolved = resolve_request(&config, &repo, &request(), &inventory, None).unwrap();
    assert!(resolved.requires_approval);
    assert_eq!(resolved.approval_lifetime, Lifetime::Workspace);
    repo.requires_approval = true;
    repo.approval_lifetime = Lifetime::Lease;
    let mut explicit = request();
    explicit.device = Some("Phone".into());
    explicit.runtime = Some("iOS".into());
    let resolved = resolve_request(&config, &repo, &explicit, &inventory, None).unwrap();
    assert_eq!(resolved.device, "phone");
    assert!(resolved.requires_approval);
    assert_eq!(resolved.approval_lifetime, Lifetime::Lease);
    inventory.runtimes[0].is_available = false;
    let error = resolve_request(&config, &repo, &request(), &inventory, None).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("runtime is not installed/available")
    );
}
