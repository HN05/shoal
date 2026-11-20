use super::*;

#[test]
fn a_cap_throttles_at_the_limit_and_kills_a_twentieth_above_it() {
    let cap = Cap::of(20 << 30);
    assert_eq!(
        properties(Some(cap)),
        ["MemoryHigh=21474836480", "MemoryMax=22548578304"]
    );
    assert_eq!(
        properties(None),
        ["MemoryHigh=infinity", "MemoryMax=infinity"]
    );
}

#[test]
fn the_slice_lies_below_the_users_service_manager() {
    let terminal = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/rex.service/t-1\n";
    assert_eq!(
        manager_group(Some(terminal), 1000),
        "user.slice/user-1000.slice/user@1000.service"
    );
    // A login session's processes lie outside the manager's group.
    let login = "0::/user.slice/user-1000.slice/session-3.scope\n";
    for own in [Some(login), None] {
        assert_eq!(
            manager_group(own, 1000),
            "user.slice/user-1000.slice/user@1000.service"
        );
    }
}

#[test]
fn a_process_in_a_scope_of_the_slice_is_in_it() {
    let scoped = "0::/user.slice/user-1000.slice/user@1000.service/shoal.slice/shoal-work.slice/run-p1-i2.scope\n";
    let terminal = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/rex.service/t-1\n";
    assert!(in_slice(scoped));
    assert!(!in_slice(terminal));
}
