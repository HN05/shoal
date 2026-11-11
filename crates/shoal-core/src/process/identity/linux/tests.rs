use super::*;

#[test]
fn a_hidden_or_exited_process_record_is_unavailable_not_an_error() {
    for errno in [libc::ENOENT, libc::ESRCH, libc::EACCES, libc::EPERM] {
        assert!(process_record_unavailable(Some(errno)), "{errno}");
    }
    assert!(!process_record_unavailable(Some(libc::EIO)));
    assert!(!process_record_unavailable(None));
}

#[test]
fn empty_environment_waits_only_for_a_loading_image() {
    let stat = |flags: u64, env_end: u64| {
        let mut fields = vec![0; 50];
        fields[6] = flags;
        fields[48] = env_end;
        let fields: Vec<_> = fields.iter().map(u64::to_string).collect();
        format!("7 (odd) name) S {}", fields[1..].join(" "))
    };
    assert!(matches!(stat_image(&stat(0, 0)).unwrap(), Image::Loading));
    assert!(matches!(
        stat_image(&stat(0, 4096)).unwrap(),
        Image::Settled
    ));
    assert!(matches!(stat_image(&stat(0x4, 0)).unwrap(), Image::Ended));
    assert!(matches!(
        stat_image(&stat(0x20_0000, 0)).unwrap(),
        Image::Ended
    ));
}

#[test]
fn a_command_line_splits_at_each_nul() {
    assert_eq!(
        split_command_line(b"sleep\x00300\0a b\0"),
        [b"sleep".to_vec(), b"300".to_vec(), b"a b".to_vec()]
    );
    assert_eq!(
        split_command_line(b"sh\0\0x\0"),
        [b"sh".to_vec(), Vec::new(), b"x".to_vec()]
    );
    // A process that rewrote its arguments may leave no terminating NUL.
    assert_eq!(
        split_command_line(b"nginx: worker"),
        [b"nginx: worker".to_vec()]
    );
    assert!(split_command_line(b"").is_empty());
}

#[test]
fn tty_nr_names_pseudo_terminals_and_consoles() {
    let tty_nr = |major: u32, minor: u32| (major << 8) | (minor & 0xff) | ((minor & !0xff) << 12);
    assert_eq!(
        terminal_device(tty_nr(136, 3)),
        Some(PathBuf::from("/dev/pts/3"))
    );
    assert_eq!(
        terminal_device(tty_nr(136, 300)),
        Some(PathBuf::from("/dev/pts/300"))
    );
    assert_eq!(
        terminal_device(tty_nr(137, 2)),
        Some(PathBuf::from("/dev/pts/258"))
    );
    assert_eq!(
        terminal_device(tty_nr(4, 1)),
        Some(PathBuf::from("/dev/tty1"))
    );
    // A serial port, the console and no terminal at all.
    assert_eq!(terminal_device(tty_nr(4, 64)), None);
    assert_eq!(terminal_device(tty_nr(5, 1)), None);
    assert_eq!(terminal_device(0), None);
}

#[test]
fn elapsed_time_counts_from_the_start_tick() {
    let uptime = Duration::from_secs(10);
    assert_eq!(
        elapsed_since(250, 100, uptime),
        Duration::from_millis(7_500)
    );
    assert_eq!(elapsed_since(2_000, 100, uptime), Duration::ZERO);
}
