use serde_json::json;

use super::{Availability, AvailabilityReason, Classified};

#[test]
fn an_availability_carries_its_reason_beside_it() {
    let connected = Classified {
        availability: Availability::Intermittent,
        reason: AvailabilityReason::Connected { percent: 87 },
    };
    let wire = json!({"availability": "intermittent", "reason": "connected", "percent": 87});
    assert_eq!(serde_json::to_value(connected).unwrap(), wire);
    assert_eq!(serde_json::from_value::<Classified>(wire).unwrap(), connected);
    let new = json!({"availability": "always_on", "reason": "new", "battery": false});
    let new: Classified = serde_json::from_value(new).unwrap();
    assert_eq!(
        new.reason,
        AvailabilityReason::New {
            battery: Some(false)
        }
    );
}

#[test]
fn the_reason_shows_the_share_it_computed() {
    let reason = AvailabilityReason::Connected { percent: 87 };
    assert!(reason.to_string().contains("87%"), "{reason}");
}
