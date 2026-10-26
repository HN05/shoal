use super::*;

#[test]
fn cpu_usage_measures_all_cores_without_guest_double_counting() {
    let first = linux_cpu("cpu 100 0 100 800 0 0 0 0 100 0\ncpu0 1 2 3 4").unwrap();
    let next = linux_cpu("cpu 180 0 110 805 5 0 0 0 180 0").unwrap();
    assert_eq!(next.used_percent_since(first), Some(90.0));
    assert_eq!(first.used_percent_since(first), None);
    assert_eq!(first.used_percent_since(next), None);
    for text in [
        "cpu0 1 2 3 4",
        "cpu 1 2 3",
        "cpu x 2 3 4",
        "cpu 18446744073709551615 1 0 0",
    ] {
        assert!(linux_cpu(text).is_err(), "{text}");
    }
}
