use super::*;

#[test]
fn a_limit_stands_until_its_named_end_or_for_a_while_without_one() {
    let now = UnixTime::from_seconds(1_000);
    let named = UpstreamLimit {
        limit: ServiceLimit::RateLimited,
        until: Some(UnixTime::from_seconds(5_000)),
    };
    assert_eq!(standing(named, now), named);
    let unnamed = UpstreamLimit {
        limit: ServiceLimit::Exhausted,
        until: None,
    };
    assert_eq!(standing(unnamed, now).until, Some(now + UNNAMED_LIMIT_FOR));
}
