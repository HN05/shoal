use super::*;

#[test]
fn every_host_keeps_sessions_under_its_name() {
    for &name in HostName::ALL {
        if let Some(host) = host(name) {
            assert_eq!(host.name(), name);
        }
    }
    assert!(host(HostName::Terminal).is_none());
}
