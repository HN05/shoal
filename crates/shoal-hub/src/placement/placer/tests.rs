use shoal_protocol::hub::placement::Unanswered;
use shoal_schedule::placement::{
    Placed, Placement, Reason,
    snapshot::{Request, Snapshot},
};

use std::time::Duration;

use super::Placers;

/// How long the hub waits for the placer.
const WAIT: Duration = Duration::from_secs(2);

fn snapshot() -> Snapshot {
    Snapshot {
        request: Request {
            origin: None,
            base: None,
            agent: "claude".into(),
            mode: "chat".into(),
            heavy: false,
            caller: "laptop".into(),
            parent: None,
        },
        machines: Vec::new(),
    }
}

fn on(machine: &str) -> Placement {
    Placement {
        ranked: vec![Placed {
            machine: machine.into(),
            reason: Reason::Local,
        }],
        skipped: Vec::new(),
    }
}

#[tokio::test]
async fn the_registered_placer_answers_by_the_number_its_snapshot_came_with() {
    let placers = Placers::default();
    let mut registration = placers.register();
    let snapshot = snapshot();
    let asking = placers.ask(&snapshot, WAIT);
    let answering = async {
        let asked = registration.asked.recv().await.unwrap();
        assert_eq!(asked.snapshot, snapshot);
        assert!(placers.answer(asked.number, on("server")));
    };
    let (placed, ()) = tokio::join!(asking, answering);
    assert_eq!(placed, Ok(on("server")));
}

#[tokio::test]
async fn without_a_placer_the_hub_hears_none_is_registered() {
    let placers = Placers::default();
    assert_eq!(
        placers.ask(&snapshot(), WAIT).await,
        Err(Unanswered::NoPlacer)
    );
    // A registration that ended leaves none.
    drop(placers.register());
    assert_eq!(
        placers.ask(&snapshot(), WAIT).await,
        Err(Unanswered::NoPlacer)
    );
}

#[tokio::test(start_paused = true)]
async fn a_placer_that_hangs_up_leaves_what_it_was_asked_without_a_placer() {
    let placers = Placers::default();
    let mut registration = placers.register();
    let snapshot = snapshot();
    let asking = placers.ask(&snapshot, WAIT);
    let hanging_up = async {
        registration.asked.recv().await.unwrap();
        drop(registration);
    };
    let (placed, ()) = tokio::join!(asking, hanging_up);
    assert_eq!(placed, Err(Unanswered::NoPlacer));
}

#[tokio::test(start_paused = true)]
async fn a_placer_that_does_not_answer_in_time_is_late_and_its_answer_refused() {
    let placers = Placers::default();
    let mut registration = placers.register();
    assert_eq!(placers.ask(&snapshot(), WAIT).await, Err(Unanswered::Late));
    let asked = registration.asked.recv().await.unwrap();
    assert!(!placers.answer(asked.number, on("server")));
}

#[tokio::test]
async fn a_later_registration_takes_over_from_the_one_before() {
    let placers = Placers::default();
    let first = placers.register();
    let mut second = placers.register();
    // The first one ending leaves the second registered.
    drop(first);
    let snapshot = snapshot();
    let asking = placers.ask(&snapshot, WAIT);
    let answering = async {
        let asked = second.asked.recv().await.unwrap();
        placers.answer(asked.number, on("desk"));
    };
    let (placed, ()) = tokio::join!(asking, answering);
    assert_eq!(placed, Ok(on("desk")));
}
