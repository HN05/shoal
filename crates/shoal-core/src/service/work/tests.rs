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
