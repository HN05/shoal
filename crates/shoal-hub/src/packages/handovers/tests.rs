use shoal_schedule::placement::{Placed, Placement, Reason, snapshot::Lifetime};

use super::*;
use crate::{
    packages::tests::{hub, kept, report},
    stand_in::{self, HANG, StandIn},
    streams::Incoming,
    test_support::linked,
    time::UnixTime,
};

/// A start of the laptop's agent, as a hand-over places it.
fn start() -> Request {
    Request {
        origin: None,
        base: None,
        agent: "claude".into(),
        mode: "worker".into(),
        heavy: true,
        lifetime: Lifetime::Lasting,
        caller: "laptop".into(),
        parent: None,
    }
}

/// What the hub writes back as the laptop hands its space `space` over
/// with the package `bytes`, as a chat in the worktree of the workspace
/// `within` when it names one, and how it ends the stream.
async fn hand_over(
    hub: &Arc<Hub>,
    (space, within): (&str, Option<&str>),
    bytes: &[u8],
) -> (Vec<u8>, Result<()>) {
    let link = linked();
    let (mut answer, mut package): (Incoming, Outgoing) = link.opener.streams().open_duplex();
    let sent = Sent {
        member: MemberId::from("laptop".to_owned()),
        space: SpaceId::from(space.to_owned()),
        built_at: UnixTime::now(),
        arrived_at: UnixTime::now(),
        continuation: Continuation {
            start: Some(start()),
            within: within.map(|id| SpaceId::from(id.to_owned())),
        },
    };
    let streams = (link.writer.streams(), answer.number());
    let daemon: Arc<dyn Daemon> = Arc::new(StandIn::default());
    hub.take_handover(&daemon, sent, streams).await.unwrap();
    package.write(bytes).await.unwrap();
    package.end(None).await;
    let mut written = Vec::new();
    loop {
        match answer.next().await {
            Ok(Some(bytes)) => written.extend(bytes),
            Ok(None) => return (written, Ok(())),
            Err(error) => return (written, Err(error)),
        }
    }
}

#[tokio::test]
async fn the_hub_keeps_a_handed_over_package_and_answers_where_its_space_goes() {
    let root = tempfile::tempdir().unwrap();
    let config = "[machine.hub]\nplacement = \"external\"\n";
    let hub = hub(&root, config, &["laptop"]).await;
    report(&hub, "laptop", &["plan"]).await;
    let own = hub.own_member().await.unwrap();
    stand_in::describe(&hub, &own, &["claude"]);
    let mut registration = hub.placers.register();
    let answering = async {
        let asked = registration.asked.recv().await.unwrap();
        // The machine shutting down is no place for its space.
        let machines = &asked.snapshot.machines;
        assert!(
            machines
                .iter()
                .all(|machine| machine.id.as_str() != "laptop")
        );
        assert_eq!(asked.snapshot.request.caller, own);
        let placement = Placement {
            ranked: vec![Placed {
                machine: own.clone(),
                reason: Reason::Caller {
                    lifetime: Lifetime::Lasting,
                },
            }],
            skipped: Vec::new(),
        };
        assert!(hub.placers.answer(asked.number, placement));
    };
    let ((written, ended), ()) = tokio::join!(hand_over(&hub, ("plan", None), b"plan"), answering);
    ended.unwrap();
    let destination: Option<Destination> = serde_json::from_slice(&written).unwrap();
    let Some(Destination::Machine(decision)) = destination else {
        panic!("{destination:?}");
    };
    assert_eq!(decision.machine, own);
    assert_eq!(kept(&hub, "plan").as_deref(), Some(&b"plan"[..]));
    // Only the laptop's word that it leaves the space continues it, once.
    let (laptop, plan) = (
        MemberId::from("laptop".to_owned()),
        SpaceId::from("plan".to_owned()),
    );
    let daemon: Arc<dyn Daemon> = Arc::new(StandIn::default());
    hub.hear_release(&daemon, &laptop, &plan).unwrap();
    assert!(hub.hear_release(&daemon, &laptop, &plan).is_err());
    // The stand-in continues nothing, so the user hears the space stays.
    let failed = tokio::time::timeout(HANG, async {
        loop {
            let alerts = hub.notifications.list(false, 50).await.unwrap();
            if let Some(alert) =
                (alerts.into_iter()).find(|alert| alert.kind == AlertKind::HandoverFailed)
            {
                return alert;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    let failed = failed.await.unwrap();
    assert!(
        failed.message.contains("stays on laptop"),
        "{}",
        failed.message
    );
    assert!(
        !hub.handovers
            .handed_to(&SpaceId::from("plan".to_owned()), &own)
    );
    // The laptop is shutting down, so it retires nothing until it says it
    // stays, as once its shutdown was cancelled.
    assert!(hub.handovers.is_leaving(&laptop));
    hub.hear_stay(&laptop);
    assert!(!hub.handovers.is_leaving(&laptop));
}

#[tokio::test]
async fn a_chat_in_a_workspace_s_worktree_follows_that_workspace() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop"]).await;
    report(&hub, "laptop", &["fix", "talk"]).await;
    let (laptop, fix, talk) = (
        MemberId::from("laptop".to_owned()),
        SpaceId::from("fix".to_owned()),
        SpaceId::from("talk".to_owned()),
    );
    let decision = Decision {
        machine: "desk".into(),
        name: "desk".into(),
        reason: Reason::Caller {
            lifetime: Lifetime::Lasting,
        },
        policy: shoal_schedule::placement::Policy::Balanced,
        unanswered: None,
        skipped: Vec::new(),
    };
    let desk = Destination::Machine(decision);
    hub.handovers
        .place(&fix, (&laptop, None, Some(start())), desk.clone());
    // The chat goes where its workspace goes, whatever placement says.
    let (written, ended) = hand_over(&hub, ("talk", Some("fix")), b"talk").await;
    ended.unwrap();
    let placed: Option<Destination> = serde_json::from_slice(&written).unwrap();
    assert_eq!(placed, Some(desk));
    // Its release waits for its workspace's, and its continuing for the
    // workspace's to end.
    let daemon: Arc<dyn Daemon> = Arc::new(StandIn::default());
    assert!(hub.hear_release(&daemon, &laptop, &talk).is_err());
    hub.handovers.release(&fix, (&laptop, None)).unwrap();
    let after = hub.handovers.release(&talk, (&laptop, None)).unwrap().after;
    let mut after = after.expect("the chat waits for its workspace");
    // Even once the laptop stays, as after a cancelled shutdown.
    hub.handovers.stays(&laptop);
    assert!(!after.has_changed().unwrap());
    hub.handovers.continued(&fix);
    assert!(*after.borrow_and_update());
}

/// Hand the laptop's space `plan` over to a hub whose own machine cannot
/// launch its agent and that has no other machine connected, and release
/// it there.
async fn keep_on_hub(hub: &Arc<Hub>) {
    let own = hub.own_member().await.unwrap();
    stand_in::describe(hub, &own, &[]);
    let (written, ended) = hand_over(hub, ("plan", None), b"plan").await;
    ended.unwrap();
    let destination: Option<Destination> = serde_json::from_slice(&written).unwrap();
    assert_eq!(destination, Some(Destination::Hub));
    let daemon: Arc<dyn Daemon> = Arc::new(StandIn::default());
    let (laptop, plan) = (
        MemberId::from("laptop".to_owned()),
        SpaceId::from("plan".to_owned()),
    );
    hub.hear_release(&daemon, &laptop, &plan).unwrap();
}

/// The first alert of `kind` the hub raises.
async fn alerted(hub: &Hub, kind: AlertKind) -> String {
    let raised = tokio::time::timeout(HANG, async {
        loop {
            let alerts = hub.notifications.list(false, 50).await.unwrap();
            if let Some(alert) = alerts.into_iter().find(|alert| alert.kind == kind) {
                return alert.message;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    raised.await.unwrap()
}

#[tokio::test]
async fn the_hub_keeps_a_space_no_machine_can_take_until_one_connects() {
    use shoal_protocol::hub::availability::{AvailabilitySetting, Description};

    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop", "desk"]).await;
    report(&hub, "laptop", &["plan"]).await;
    keep_on_hub(&hub).await;
    let desk = MemberId::from("desk".to_owned());
    let _link = hub.links.register(&desk, "desk").unwrap();
    let description = Description {
        battery: false,
        availability: AvailabilitySetting::AlwaysOn,
    };
    hub.describe_member(&desk, description).await.unwrap();
    let room = shoal_schedule::placement::snapshot::Capacity {
        agents: vec!["claude".into()],
        heavy_free: 1,
        light_free: true,
        ..Default::default()
    };
    let changes = shoal_protocol::hub::directory::Report::Changes {
        changed: Vec::new(),
        removed: Vec::new(),
    };
    let link = hub.links.token(&desk);
    hub.capacities.keep((&desk, link), &changes, Some(room), None);
    // The stand-in link continues nothing, so the user hears the hub tried
    // the desk.
    let failed = alerted(&hub, AlertKind::HandoverFailed).await;
    assert!(failed.contains("machine desk"), "{failed}");
}

#[tokio::test]
async fn a_space_the_hub_keeps_stays_on_its_machine_once_that_machine_is_back() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop"]).await;
    report(&hub, "laptop", &["plan"]).await;
    keep_on_hub(&hub).await;
    let (laptop, plan) = (
        MemberId::from("laptop".to_owned()),
        SpaceId::from("plan".to_owned()),
    );
    hub.hear_stay(&laptop);
    let stays = alerted(&hub, AlertKind::HandoverFailed).await;
    assert!(stays.contains("laptop is back"), "{stays}");
    assert!(!hub.handovers.keeps(&plan, &laptop));
    // A placement that ends after the laptop came back sends it nowhere.
    let desk = Decision {
        machine: "desk".into(),
        name: "desk".into(),
        reason: Reason::Local,
        policy: shoal_schedule::placement::Policy::Balanced,
        unanswered: None,
        skipped: Vec::new(),
    };
    assert!(!hub.handovers.placed_later((&plan, &laptop), desk, || None));
}

#[tokio::test]
async fn the_hub_keeps_a_space_it_cannot_launch_under_the_local_policy_too() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "[machine.hub]\nplacement = \"local\"\n", &["laptop"]).await;
    report(&hub, "laptop", &["plan"]).await;
    keep_on_hub(&hub).await;
}

#[tokio::test]
async fn a_space_the_hub_keeps_is_kept_no_longer_once_another_machine_takes_it_over() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop", "desk"]).await;
    report(&hub, "laptop", &["plan"]).await;
    keep_on_hub(&hub).await;
    let (laptop, desk, plan) = (
        MemberId::from("laptop".to_owned()),
        MemberId::from("desk".to_owned()),
        SpaceId::from("plan".to_owned()),
    );
    // As `shoal restore` on the desk takes it over from the package.
    (hub.record_space_claim(&desk, &plan, (None, true)))
        .await
        .unwrap();
    assert!(!hub.handovers.keeps(&plan, &laptop));
}

#[tokio::test]
async fn another_machine_s_claim_is_refused_once_the_hub_placed_a_space_it_kept() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop", "desk", "server"]).await;
    report(&hub, "laptop", &["plan"]).await;
    keep_on_hub(&hub).await;
    let (laptop, server, plan) = (
        MemberId::from("laptop".to_owned()),
        MemberId::from("server".to_owned()),
        SpaceId::from("plan".to_owned()),
    );
    let desk = Decision {
        machine: "desk".into(),
        name: "desk".into(),
        reason: Reason::Local,
        policy: shoal_schedule::placement::Policy::Balanced,
        unanswered: None,
        skipped: Vec::new(),
    };
    assert!(hub.handovers.placed_later((&plan, &laptop), desk, || None));
    // A restore on the server while the desk continues it.
    let claimed = hub.record_space_claim(&server, &plan, (None, true)).await;
    assert!(claimed.is_err());
    assert!(hub.handovers.handed_to(&plan, &"desk".into()));
}

#[tokio::test]
async fn a_placement_after_the_machine_registered_a_new_link_sends_its_space_nowhere() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop"]).await;
    report(&hub, "laptop", &["plan"]).await;
    keep_on_hub(&hub).await;
    let (laptop, plan) = (
        MemberId::from("laptop".to_owned()),
        SpaceId::from("plan".to_owned()),
    );
    // Its link registered, though the hub has not taken its stay yet.
    let _link = hub.links.register(&laptop, "laptop").unwrap();
    let desk = Decision {
        machine: "desk".into(),
        name: "desk".into(),
        reason: Reason::Local,
        policy: shoal_schedule::placement::Policy::Balanced,
        unanswered: None,
        skipped: Vec::new(),
    };
    let link = || hub.links.token(&laptop);
    assert!(!hub.handovers.placed_later((&plan, &laptop), desk, link));
    assert!(hub.handovers.keeps(&plan, &laptop));
}
