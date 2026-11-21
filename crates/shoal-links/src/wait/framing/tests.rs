use shoal_forge_types::updates::UpdateKind;

use super::*;

fn large_update(n: usize, delivery: usize) -> Update {
    Update {
        url: "https://forge.example/team/repo/pulls/2".into(),
        kind: UpdateKind::Comment,
        message: format!("{n}{}", "x".repeat(4096)),
        delivery: delivery.to_string(),
        check: None,
        workspace: None,
    }
}

#[test]
fn combined_updates_fit_one_response_in_whole_deliveries() {
    const LIMIT: usize = MAX_FRAME - 256;
    let batch = |deliveries: fn(usize) -> usize| {
        (0..63)
            .map(|n| {
                (
                    SpaceId::from("child".to_owned()),
                    large_update(n, deliveries(n)),
                )
            })
            .collect::<Batch>()
    };
    // 21 deliveries of three updates each, more than one response holds.
    let Cut::Between(fitting) = frame_cut(&batch(|n| n / 3), LIMIT) else {
        panic!("whole deliveries fit");
    };
    assert!(fitting > 0 && fitting < 63 && fitting % 3 == 0);
    let sent = batch(|n| n / 3)[..fitting].to_vec();
    assert!(serde_json::to_vec(&sent).unwrap().len() < MAX_FRAME);
    // One delivery larger than a response is cut inside it.
    let Cut::Inside(fitting) = frame_cut(&batch(|_| 0), LIMIT) else {
        panic!("a delivery too large for a response is cut");
    };
    assert!(fitting > 0 && fitting < 63);
    assert_eq!(frame_cut(&Vec::new(), LIMIT), Cut::Between(0));
}
