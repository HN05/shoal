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
