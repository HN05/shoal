use super::*;
use std::{hint::black_box, ops::RangeInclusive, time::Instant};

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

fn scan(reservations: &[PortReservation], range: RangeInclusive<u16>) -> Option<u16> {
    range
        .into_iter()
        .find(|port| !reservations.iter().any(|p| p.port == *port))
}

fn indexed(reservations: &[PortReservation], range: RangeInclusive<u16>) -> Option<u16> {
    first_available(reservations, range, |_| Ok::<_, ()>(true)).unwrap()
}

/// Includes construction and destruction per allocation; excludes SQL and TCP.
/// Run: cargo test --release benchmark_port_membership -- --ignored --nocapture
#[test]
#[ignore = "focused membership benchmark"]
fn benchmark_port_membership() {
    fn measure(mut operation: impl FnMut() -> Option<u16>, iterations: u32) -> f64 {
        let start = Instant::now();
        for _ in 0..iterations {
            black_box(operation());
        }
        start.elapsed().as_nanos() as f64 / f64::from(iterations)
    }
    println!("reservations,case,scan_ns,lazy_bitmap_ns");
    for count in [0, 16, 32, 33, 100, 1000, 10000] {
        for case in ["dense", "first-free", "sparse", "exhausted"] {
            let ports: Vec<u16> = match case {
                "first-free" => (20000..20000 + count).collect(),
                "sparse" => (1000..1000 + 2 * count).step_by(2).collect(),
                _ => (1000..1000 + count).collect(),
            };
            let reservations = reservations(ports);
            let end = if case == "exhausted" && count > 0 {
                999 + count
            } else {
                1000 + count
            };
            let range = 1000..=end;
            assert_eq!(
                scan(&reservations, range.clone()),
                indexed(&reservations, range.clone())
            );
            let iterations = if count >= 1000 && matches!(case, "dense" | "exhausted") {
                10
            } else {
                10000
            };
            let mut scans = Vec::new();
            let mut indexed_samples = Vec::new();
            for sample in 0..9 {
                let baseline = || {
                    measure(
                        || scan(black_box(&reservations), black_box(range.clone())),
                        iterations,
                    )
                };
                let bitmap = || {
                    measure(
                        || indexed(black_box(&reservations), black_box(range.clone())),
                        iterations,
                    )
                };
                let (a, b) = if sample % 2 == 0 {
                    (baseline(), bitmap())
                } else {
                    let b = bitmap();
                    (baseline(), b)
                };
                scans.push(a);
                indexed_samples.push(b);
            }
            scans.sort_by(f64::total_cmp);
            indexed_samples.sort_by(f64::total_cmp);
            println!("{count},{case},{:.1},{:.1}", scans[4], indexed_samples[4]);
        }
    }
}
