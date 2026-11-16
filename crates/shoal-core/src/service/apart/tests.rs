use super::*;

#[test]
fn a_user_service_runs_in_the_slice_its_control_group_sits_in() {
    let cgroup = "0::/user.slice/user-1000.slice/user@1000.service/workloads.slice/shoal.service\n";
    assert_eq!(
        Service::of(cgroup),
        Some(Service {
            slice: Some("workloads.slice".into())
        })
    );
    let unsliced = "0::/user.slice/user-1000.slice/user@1000.service/shoal.service\n";
    assert_eq!(Service::of(unsliced), Some(Service { slice: None }));
}

#[test]
fn a_process_outside_a_user_service_starts_nothing_apart() {
    let terminal = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/vte-spawn-1.scope\n";
    let system = "0::/system.slice/shoal.service\n";
    let legacy = "1:name=systemd:/user.slice/user-1000.slice/user@1000.service/shoal.service\n";
    for cgroup in [terminal, system, legacy] {
        assert_eq!(Service::of(cgroup), None, "{cgroup}");
    }
}

#[test]
fn the_launcher_starts_a_scope_in_the_services_slice() {
    let service = Service {
        slice: Some("workloads.slice".into()),
    };
    let launcher = service.launcher();
    assert!(launcher.contains(&"--scope".into()), "{launcher:?}");
    assert!(
        launcher.contains(&"--slice=workloads.slice".into()),
        "{launcher:?}"
    );
    assert!(
        launcher.contains(&"--property=OOMPolicy=continue".into()),
        "{launcher:?}"
    );
    assert_eq!(launcher.last(), Some(&"--".into()));
}

#[test]
fn only_a_systemd_that_can_leave_arguments_unexpanded_starts_scopes() {
    assert!(keeps_arguments("systemd 262 (262.1-1.fc45)\n+PAM +AUDIT"));
    assert!(keeps_arguments("systemd 254 (254.5-2)"));
    assert!(!keeps_arguments("systemd 252 (252.30-1~deb12u2)"));
    assert!(!keeps_arguments("unexpected"));
}
