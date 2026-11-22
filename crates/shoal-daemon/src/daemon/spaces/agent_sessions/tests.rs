use super::{AgentPlaces, agent_places};
use crate::{
    model::RunningAgent,
    sessions::{HostName, Origin},
};
use shoal_ids::space_ids::SpaceId;

#[test]
fn agent_places_count_stopped_agents_as_connected() {
    let place = |name: &str| Origin::created(HostName::Zmx, name.into());
    let (running, stopped, gone) = (place("running"), place("stopped"), place("gone"));
    let agent = |space: &str, origin: Option<&Origin>, name: Option<&str>| RunningAgent {
        execution: format!("{space}-execution").into(),
        space_id: space.into(),
        agent: name.map(Into::into),
        restorable: true,
        state: None,
        interrupted: false,
        origin: origin.cloned(),
        stopped: None,
        restarted_from: Vec::new(),
    };
    let agents = [
        agent("w1", Some(&running), Some("claude")),
        RunningAgent {
            stopped: Some(crate::model::Protection {
                reason: "critical memory pressure".into(),
                resumes_when: "an update arrives for it".into(),
            }),
            ..agent("w2", Some(&stopped), Some("claude"))
        },
        agent("w3", Some(&gone), None),
        agent("w4", None, Some("codex")),
    ];
    let [w1, w2, w3] = ["w1", "w2", "w3"].map(SpaceId::from);
    assert_eq!(
        agent_places(&agents),
        AgentPlaces {
            connected: vec![(&w1, &running), (&w2, &stopped)],
            unknown: vec![(&w3, &gone)],
        }
    );
}
