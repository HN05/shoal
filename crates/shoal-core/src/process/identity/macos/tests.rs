use super::*;
use crate::process::identity::capture;

#[test]
fn process_identity_unavailable_includes_inaccessible_processes() {
    assert!(process_identity_unavailable(Some(libc::ESRCH)));
    assert!(process_identity_unavailable(Some(libc::ENOENT)));
    assert!(process_identity_unavailable(Some(libc::EPERM)));
    assert!(!process_identity_unavailable(Some(libc::EACCES)));
    assert!(!process_identity_unavailable(None));
}

#[test]
fn a_start_time_matches_the_birth_of_the_identity() -> Result<()> {
    let own = capture(std::process::id())?.context("own identity")?;
    assert_eq!(start_time(own.pid)?, Some(own.birth));
    Ok(())
}

#[test]
fn launchd_starts_a_process_only_by_a_read_and_matching_version() {
    let process = |parent_unique, original_parent_version| UniqueIdentifiers {
        uuid: [0; 16],
        unique: 2,
        parent_unique,
        id_version: 0,
        original_parent_version,
        reserved: [0; 2],
    };
    assert!(started_by_launchd(&process(1, 7), Some(7)));
    assert!(!started_by_launchd(&process(1, 7), None));
    assert!(!started_by_launchd(&process(1, 8), Some(7)));
    assert!(!started_by_launchd(&process(5, 7), Some(7)));
}

#[test]
fn an_exiting_or_exited_process_has_ended_its_image() {
    let bsd = |status: u32, flags: u32| {
        let mut bsd: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        bsd.pbi_status = status;
        bsd.pbi_flags = flags;
        bsd
    };
    // SRUN = 2; PROC_FLAG_LP64 = 0x10; PROC_FLAG_INEXIT = 0x4; SZOMB = 5.
    assert!(matches!(bsd_image(&bsd(2, 0x10)), Image::Settled));
    assert!(matches!(bsd_image(&bsd(2, 0x14)), Image::Ended));
    assert!(matches!(bsd_image(&bsd(5, 0)), Image::Ended));
}

#[test]
fn procargs_split_into_arguments_and_environment() {
    let mut record = 2_i32.to_ne_bytes().to_vec();
    record.extend_from_slice(b"/bin/sleep\0\0\0\0sleep\0a b\0HOME=/h\0A=\0\0\0junk");
    let split = split_procargs(&record).unwrap();
    assert_eq!(split.arguments, [b"sleep".to_vec(), b"a b".to_vec()]);
    assert_eq!(split.environment, [b"HOME=/h".to_vec(), b"A=".to_vec()]);

    let mut cut = 3_i32.to_ne_bytes().to_vec();
    cut.extend_from_slice(b"/bin/sleep\0sleep\x00300\0");
    assert!(split_procargs(&cut).is_err());
}
