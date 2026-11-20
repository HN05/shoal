use super::*;

#[test]
fn the_stop_order_spreads_from_the_first_score_to_the_last() {
    assert_eq!(scores(0), Vec::<i16>::new());
    assert_eq!(scores(1), [FIRST]);
    assert_eq!(scores(2), [FIRST, LAST]);
    assert_eq!(scores(5), [900, 725, 550, 375, 200]);
}

#[test]
fn only_a_transient_scope_of_its_own_counts_as_a_commands_scope() {
    let scope = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/run-p42-i43.scope\n";
    assert_eq!(
        transient_scope(scope),
        Some("user.slice/user-1000.slice/user@1000.service/app.slice/run-p42-i43.scope")
    );
    let service = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/shoal.service\n";
    assert_eq!(transient_scope(service), None);
    let terminal = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-rex.scope\n";
    assert_eq!(transient_scope(terminal), None);
    assert_eq!(transient_scope("1:name=systemd:/run-p1-i1.scope\n"), None);
    // A scope the daemon itself runs in, as a shell's, is not the job's.
    assert_eq!(own_scope(scope, service), transient_scope(scope));
    assert_eq!(own_scope(scope, scope), None);
}

#[test]
fn a_command_starts_at_the_last_score() {
    assert_eq!(START_SCORE, LAST.to_string().as_bytes());
}

#[cfg(target_os = "linux")]
#[test]
fn a_score_is_written_only_where_it_changes() {
    let pid = std::process::id();
    let own = read(pid).unwrap();
    // Raising a score needs no privilege.
    assert!(!set(pid, own).unwrap());
    let raised = own.max(LAST) + 1;
    if raised > 1000 {
        return;
    }
    assert!(set(pid, raised).unwrap());
    assert_eq!(read(pid).unwrap(), raised);
}

#[cfg(target_os = "linux")]
#[test]
fn a_scoped_process_is_scored_only_while_it_runs_in_its_scope() {
    let pid = std::process::id();
    let groups = std::fs::read_to_string("/proc/self/cgroup").unwrap();
    let Some(own) = unified(&groups) else {
        return;
    };
    let own = Path::new("/sys/fs/cgroup").join(own);
    let raised = read(pid).unwrap().max(LAST) + 1;
    if raised > 1000 {
        return;
    }
    assert!(!set_in_scope(pid, Path::new("/sys/fs/cgroup/elsewhere.scope"), raised).unwrap());
    assert!(set_in_scope(pid, &own, raised).unwrap());
    assert_eq!(read(pid).unwrap(), raised);
}
