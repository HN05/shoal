use super::*;

fn reservations(ports: impl IntoIterator<Item = u16>) -> Vec<PortReservation> {
    ports
        .into_iter()
        .map(|port| PortReservation {
            workspace_id: "workspace".into(),
            name: format!("port-{port}"),
            port,
            env_var: format!("PORT_{port}"),
            reason: Some("test reservation".into()),
        })
        .collect()
}

#[test]
fn selection_skips_reservations_and_probes_in_ascending_order() {
    for (ports, range, busy, expected, probes) in [
        (vec![], 100..=102, vec![], Some(100), vec![100]),
        (
            vec![100, 102],
            100..=104,
            vec![101],
            Some(103),
            vec![101, 103],
        ),
        (
            (200..300).collect(),
            100..=104,
            vec![],
            Some(100),
            vec![100],
        ),
        (
            (100..200).step_by(2).collect(),
            100..=200,
            vec![101],
            Some(103),
            vec![101, 103],
        ),
        (
            (100..200).collect(),
            100..=200,
            vec![],
            Some(200),
            vec![200],
        ),
        ((100..=200).collect(), 100..=200, vec![], None, vec![]),
        ((100..200).collect(), 100..=200, vec![200], None, vec![200]),
        (
            (65400..65535).collect(),
            65400..=65535,
            vec![],
            Some(65535),
            vec![65535],
        ),
    ] {
        let reservations = reservations(ports);
        let mut actual_probes = Vec::new();
        assert_eq!(
            first_available(&reservations, range, |port| -> Result<bool> {
                actual_probes.push(port);
                Ok(!busy.contains(&port))
            })
            .unwrap(),
            expected
        );
        assert_eq!(actual_probes, probes);
    }
}

#[test]
fn probe_errors_propagate_during_scans_and_bitmap_search() {
    let reservations = reservations(100..200);
    for range in [300..=300, 100..=200] {
        assert_eq!(
            first_available(&reservations, range, |_| Err::<bool, _>("probe failed")),
            Err("probe failed")
        );
    }
}
