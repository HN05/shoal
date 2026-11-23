use std::time::Duration;

use super::*;

#[test]
fn a_machine_s_time_keeps_its_distance_from_when_it_sent_on_the_hub_s_clock() {
    let received = UnixTime::from_seconds(10_000);
    let minutes = |n: u64| Duration::from_secs(n * 60);
    // The machine's clock runs 20 minutes ahead of the hub's.
    let ahead = Clocks {
        sent: received + minutes(20),
        received,
    };
    let lag = Lagging {
        space: SpaceId::from("plan".to_owned()),
        since: Some(received + minutes(15)),
    };
    let on_hub = lag.on_hub_clock(ahead).since.unwrap();
    assert_eq!(on_hub, received - minutes(5));
    // One that runs behind carries over the same way.
    let behind = Clocks {
        sent: received - minutes(20),
        received,
    };
    assert_eq!(behind.on_hub(received - minutes(25)), received - minutes(5));
}
