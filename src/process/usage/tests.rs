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
