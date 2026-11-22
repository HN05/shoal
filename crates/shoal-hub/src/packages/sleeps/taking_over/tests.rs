use serde_json::json;

use super::*;

fn id(id: &str) -> SpaceId {
    SpaceId::from(id.to_owned())
}

fn listed(machine: &str, id: &str, fields: serde_json::Value) -> Listed {
    let mut space = json!({
        "machine": machine, "id": id, "number": 1, "name": id, "kind": "chat",
        "status": "inactive", "reported_at": 0,
    });
    space
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    serde_json::from_value(space).unwrap()
}

fn start(mode: &str) -> Request {
    Request {
        origin: None,
        base: None,
        agent: "claude".into(),
        heavy: true,
        mode: mode.into(),
        caller: MemberId::from(String::new()),
        parent: None,
    }
}

/// A package kept for each of `spaces`, with a start, or for a chat the
/// workspace whose worktree it runs in.
fn kept(spaces: &[(&str, Option<&str>)]) -> HashMap<SpaceId, Continuation> {
    (spaces.iter())
        .map(|(space, within)| {
            let continuation = Continuation {
                start: Some(start(space)),
                within: within.map(id),
            };
            (id(space), continuation)
        })
        .collect()
}

fn sleeper(lags: &[(&str, Option<UnixTime>)]) -> Sleeper {
    Sleeper {
        member: MemberId::from("laptop".to_owned()),
        asleep_at: UnixTime::from_seconds(10_000),
        lags: (lags.iter())
            .map(|(space, since)| (id(space), *since))
            .collect(),
    }
}

/// The spaces of each group that goes, in order.
fn moved(groups: Vec<Group>) -> Vec<Vec<String>> {
    (groups.into_iter())
        .map(|group| group.spaces.iter().map(|id| id.to_string()).collect())
        .collect()
}

#[test]
fn a_space_with_work_goes_and_an_idle_one_stays_until_an_update_waits_for_it() {
    let directory = Directory {
        spaces: vec![
            listed("laptop", "working", json!({"agent_state": "working"})),
            listed("laptop", "queued", json!({"status": "queued"})),
            listed("laptop", "idle", json!({"agent_state": "idle"})),
            listed("laptop", "told", json!({"agent_state": "idle"})),
            listed("desk", "elsewhere", json!({"agent_state": "working"})),
        ],
        ..Directory::default()
    };
    let kept = kept(&[
        ("working", None),
        ("queued", None),
        ("idle", None),
        ("told", None),
        ("elsewhere", None),
    ]);
    let held = HashSet::from([id("told")]);
    let groups = movable(&directory, &sleeper(&[]), &kept, &held);
    assert_eq!(moved(groups), [["queued"], ["told"], ["working"]]);
}

#[test]
fn a_package_that_lags_too_far_or_by_an_unknown_time_keeps_its_space() {
    let asleep_at = sleeper(&[]).asleep_at;
    let directory = Directory {
        spaces: ["current", "behind", "far", "unknown"]
            .map(|space| listed("laptop", space, json!({"agent_state": "working"})))
            .to_vec(),
        ..Directory::default()
    };
    let kept = kept(&[
        ("current", None),
        ("behind", None),
        ("far", None),
        ("unknown", None),
    ]);
    let lags = sleeper(&[
        ("behind", Some(asleep_at - Duration::from_secs(14 * 60))),
        ("far", Some(asleep_at - LAG_LIMIT)),
        ("unknown", None),
    ]);
    let groups = movable(&directory, &lags, &kept, &HashSet::new());
    assert_eq!(moved(groups), [["behind"], ["current"]]);
}

#[test]
fn a_chat_in_a_worktree_goes_after_its_workspace_and_only_with_it() {
    let directory = Directory {
        spaces: vec![
            listed("laptop", "fix", json!({"kind": "workspace"})),
            listed("laptop", "fix-chat", json!({"agent_state": "working"})),
            listed(
                "laptop",
                "plan",
                json!({"kind": "workspace", "agent_state": "working"}),
            ),
            listed("laptop", "plan-chat", json!({})),
        ],
        ..Directory::default()
    };
    let kept = kept(&[
        ("fix", None),
        ("fix-chat", Some("fix")),
        ("plan", None),
        ("plan-chat", Some("plan")),
    ]);
    // The chat's work takes its workspace along.
    let groups = movable(&directory, &sleeper(&[]), &kept, &HashSet::new());
    assert_eq!(moved(groups), [["fix", "fix-chat"], ["plan", "plan-chat"]]);
    // A chat whose package lags too far keeps its workspace too.
    let lagging = sleeper(&[("plan-chat", None)]);
    let groups = movable(&directory, &lagging, &kept, &HashSet::new());
    assert_eq!(moved(groups), [["fix", "fix-chat"]]);
}

#[test]
fn a_space_without_a_start_or_a_package_stays() {
    let directory = Directory {
        spaces: ["started", "unstarted", "unkept"]
            .map(|space| listed("laptop", space, json!({"agent_state": "working"})))
            .to_vec(),
        ..Directory::default()
    };
    let mut kept = kept(&[("started", None), ("unstarted", None)]);
    kept.get_mut(&id("unstarted")).unwrap().start = None;
    let groups = movable(&directory, &sleeper(&[]), &kept, &HashSet::new());
    assert_eq!(moved(groups), [["started"]]);
}
