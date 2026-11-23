use super::*;

#[test]
fn a_paused_machine_says_since_when() {
    let now = UnixTime::from_seconds(1_800_000_000);
    let mut member = Member::unlisted("desk".to_owned().into());
    assert_eq!(paused(&member, now), "");
    member.capacity = Some(shoal_schedule::placement::snapshot::Capacity {
        paused: Some(now),
        ..Default::default()
    });
    assert_eq!(
        paused(&member, now),
        format!("paused {}", now.local_ago(now))
    );
}
