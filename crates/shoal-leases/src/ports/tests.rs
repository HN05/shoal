use super::*;
use crate::stand_in::Fixture;
use shoal_protocol::approvals::DecisionStatus;

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

/// A free port to prefer, which a listener held while it was picked.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.local_addr().unwrap().port()
}

/// The fixture's workspace with a port `web` that prefers `port`.
async fn fixture(port: u16, requires_approval: bool) -> Fixture {
    let repo = format!("[ports.web]\nport = {port}\nrequires_approval = {requires_approval}\n");
    Fixture::new("", &repo).await
}

/// Reserve the port `web` as `caller`.
async fn acquire(fixture: &Fixture, caller: Option<&Caller>) -> Acquisition {
    let request = PortRequest {
        reason: Some("serve the app".into()),
        ..PortRequest::default()
    };
    let (leases, selector) = (&fixture.leases, fixture.selector());
    (leases.acquire_port(&fixture.daemon, &selector, "web".into(), request, caller))
        .await
        .unwrap()
}

async fn release(fixture: &Fixture) {
    let (leases, selector) = (&fixture.leases, fixture.selector());
    (leases.release_port(&fixture.daemon, &selector, "web".into()))
        .await
        .unwrap();
}

async fn reserved(fixture: &Fixture) -> Vec<u16> {
    let listed = fixture.leases.list_ports(&fixture.daemon, None).await;
    listed.unwrap().into_iter().map(|port| port.port).collect()
}

fn granted(acquisition: Acquisition) -> PortReservation {
    match acquisition {
        Acquisition::Allocation(Allocation::Granted(reservation)) => reservation,
        _ => panic!("the port was not reserved"),
    }
}

#[tokio::test]
async fn a_reservation_takes_its_preferred_port_and_repeating_it_returns_the_same() {
    let port = free_port();
    let fixture = fixture(port, false).await;

    let first = granted(acquire(&fixture, None).await);
    let again = granted(acquire(&fixture, None).await);

    assert_eq!(
        (first.port, first.env_var.as_str()),
        (port, "SHOAL_PORT_WEB")
    );
    assert_eq!(again.port, port);
    assert_eq!(reserved(&fixture).await, [port]);
    release(&fixture).await;
    assert!(reserved(&fixture).await.is_empty());
    assert_eq!(fixture.daemon.touched.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn a_taken_preferred_port_is_only_suggested_and_the_conflict_notified() {
    let listener = std::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let fixture = fixture(port, false).await;

    let Acquisition::Suggested(suggestion) = acquire(&fixture, None).await else {
        panic!("a taken port was reserved");
    };

    assert_eq!(suggestion.requested_port, port);
    assert_ne!(suggestion.suggested_port, port);
    let notified = fixture.notified().await;
    assert_eq!(notified.len(), 1);
    assert_eq!(notified[0].0, NotificationKind::PortConflict);
    assert!(
        notified[0]
            .1
            .contains(&suggestion.suggested_port.to_string())
    );
    assert!(reserved(&fixture).await.is_empty());
}

#[tokio::test]
async fn a_scoped_reservation_waits_for_its_approval_and_the_user_needs_none() {
    let port = free_port();
    let fixture = fixture(port, true).await;
    let caller = Caller::of_space(fixture.workspace.id.clone());

    let Acquisition::Allocation(Allocation::Approval(request)) =
        acquire(&fixture, Some(&caller)).await
    else {
        panic!("a scoped reservation did not ask for approval");
    };
    assert_eq!(request.target, Target::Port("web".into()));
    assert_eq!(fixture.daemon.announced.lock().unwrap().len(), 1);
    assert!(reserved(&fixture).await.is_empty());

    let target = Target::Port("web".into());
    fixture
        .grants
        .decide(&target, "web", DecisionStatus::Approved);
    assert_eq!(granted(acquire(&fixture, Some(&caller)).await).port, port);
    release(&fixture).await;
    assert_eq!(granted(acquire(&fixture, None).await).port, port);
}
