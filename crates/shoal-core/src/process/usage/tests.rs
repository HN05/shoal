use super::*;

#[test]
fn linux_readings_count_waited_children_and_resident_pages() {
    let stat = "42 (a (b) c) S 7 42 42 0 -1 4194560 1 2 3 4 \
                    100 50 20 30 20 0 1 0 999 1000 25 18446744073709551615";
    assert_eq!(
        linux_reading(stat, 100, 4096).unwrap(),
        Reading {
            parent: 7,
            cpu: 1_500_000_000,
            reaped: 500_000_000,
            memory: 25 * 4096,
        }
    );
    assert!(linux_reading("42 (a) S 7 42", 100, 4096).is_err());
    assert!(linux_reading(&stat.replace(" 100 ", " x "), 100, 4096).is_err());
}

#[test]
fn a_process_s_swapped_pages_come_from_its_status() {
    let status = "Name:\tcargo\nVmRSS:\t  2048 kB\nVmSwap:\t  1536 kB\nThreads:\t4\n";
    assert_eq!(vm_swap(status), Some(1536 * 1024));
    assert_eq!(vm_swap("Name:\tkthreadd\n"), None);
}

#[test]
fn a_tree_counts_its_live_processes_and_nothing_once_they_ended() {
    use std::os::unix::process::CommandExt as _;
    let mut child = std::process::Command::new("sh")
        .args(["-c", "sleep 30 & wait"])
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = child.id();
    let own = read(pid).unwrap().unwrap().memory;
    // The shell starts its sleep a moment after it starts itself.
    let started = std::time::Instant::now();
    while tree_memory(pid).unwrap() <= own && started.elapsed().as_secs() < 30 {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(tree_memory(pid).unwrap() > own);
    // SAFETY: killpg takes no pointers; the group is the shell's own.
    unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
    child.wait().unwrap();
    assert_eq!(tree_memory(pid).unwrap(), 0);
}
