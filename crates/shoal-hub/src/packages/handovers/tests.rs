use shoal_schedule::placement::{Placed, Placement, Reason};

use super::*;
use crate::{
    packages::tests::{hub, kept, report},
    stand_in::{HANG, StandIn},
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
    };
    let streams = (link.writer.streams(), answer.number());
    let daemon: Arc<dyn Daemon> = Arc::new(StandIn::default());
    let within = within.map(|id| SpaceId::from(id.to_owned()));
    hub.take_handover(&daemon, (sent, within), start(), streams)
        .await
        .unwrap();
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
                reason: Reason::Caller,
            }],
            skipped: Vec::new(),
        };
        assert!(hub.placers.answer(asked.number, placement));
    };
    let ((written, ended), ()) = tokio::join!(hand_over(&hub, ("plan", None), b"plan"), answering);
    ended.unwrap();
    let decision: Option<Decision> = serde_json::from_slice(&written).unwrap();
    assert_eq!(decision.unwrap().machine, own);
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
        reason: Reason::Caller,
        policy: shoal_schedule::placement::Policy::Balanced,
        unanswered: None,
        skipped: Vec::new(),
    };
    hub.handovers.place(&fix, (&laptop, None), decision);
    // The chat goes where its workspace goes, whatever placement says.
    let (written, ended) = hand_over(&hub, ("talk", Some("fix")), b"talk").await;
    ended.unwrap();
    let placed: Option<Decision> = serde_json::from_slice(&written).unwrap();
    assert_eq!(placed.unwrap().name, "desk");
    // Its release waits for its workspace's, and its continuing for the
    // workspace's to end.
    let daemon: Arc<dyn Daemon> = Arc::new(StandIn::default());
    assert!(hub.hear_release(&daemon, &laptop, &talk).is_err());
    hub.handovers.release(&fix, &laptop).unwrap();
    let after = hub.handovers.release(&talk, &laptop).unwrap().after;
    let mut after = after.expect("the chat waits for its workspace");
    // Even once the laptop stays, as after a cancelled shutdown.
    hub.handovers.stays(&laptop);
    assert!(!after.has_changed().unwrap());
    hub.handovers.continued(&fix);
    assert!(*after.borrow_and_update());
}
