use super::*;

#[test]
fn a_pid_that_names_no_process_shows_nothing() {
    for pid in [0, u32::MAX] {
        assert_eq!(arguments(pid), None);
        assert_eq!(elapsed(pid).unwrap(), None);
    }
}
