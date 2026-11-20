use super::*;
use std::time::Duration;

fn at(seconds: i64) -> UnixTime {
    UnixTime::from_seconds(seconds)
}

fn agent(execution: &str, space: &str) -> RunningAgent {
    RunningAgent {
        execution: execution.into(),
        space: space.into(),
        light: false,
        running_for: Duration::from_secs(100),
        interacted_at: None,
        idle_since: None,
    }
}

fn order(running: &Running, ranked: &[&str]) -> Vec<String> {
    let ranked: Vec<SpaceId> = ranked.iter().map(|&space| space.into()).collect();
    names(StopOrder::new(running, &ranked).stops().iter())
}

fn names<'a>(stops: impl Iterator<Item = &'a Stop>) -> Vec<String> {
    stops
        .map(|stop| match &stop.work {
            Work::Job => format!("job {}", stop.space),
            Work::Demo(execution) => format!("demo {execution}"),
            Work::Agent { execution, .. } => execution.to_string(),
        })
        .collect()
}

#[test]
fn jobs_go_newest_first_then_demos_then_agents() {
    let running = Running {
        jobs: vec![
            RunningJob {
                space: "old".into(),
                started: at(10),
            },
            RunningJob {
                space: "new".into(),
                started: at(20),
            },
        ],
        demos: vec![RunningDemo {
            execution: "web".into(),
            space: "w".into(),
            started: at(5),
        }],
        agents: vec![agent("a", "w")],
        ..Running::default()
    };
    assert_eq!(
        order(&running, &[]),
        ["job new", "job old", "demo web", "a"]
    );
}

#[test]
fn an_agent_goes_after_every_agent_below_it_light_ones_included() {
    let mut coordinator = agent("coordinator", "top");
    // It costs the least on its own: never interacted with and idle.
    coordinator.idle_since = Some(at(1));
    let mut child = agent("child", "middle");
    child.light = true;
    child.interacted_at = Some(at(50));
    let mut grandchild = agent("grandchild", "bottom");
    grandchild.interacted_at = Some(at(60));
    let running = Running {
        agents: vec![coordinator, child, grandchild, agent("other", "elsewhere")],
        parents: HashMap::from([
            ("middle".into(), "top".into()),
            ("bottom".into(), "middle".into()),
        ]),
        ..Running::default()
    };
    assert_eq!(
        order(&running, &[]),
        ["other", "grandchild", "child", "coordinator"]
    );
}

#[test]
fn among_leaves_heavy_goes_before_light_then_the_queues_keys() {
    let mut light = agent("light", "l");
    light.light = true;
    let mut interacted = agent("interacted", "i");
    interacted.interacted_at = Some(at(5));
    let mut idle = agent("idle", "d");
    idle.idle_since = Some(at(3));
    let ranked_first = agent("ranked first", "first");
    let ranked_last = agent("ranked last", "last");
    let running = Running {
        agents: vec![light, interacted, ranked_first, idle, ranked_last],
        ..Running::default()
    };
    assert_eq!(
        order(&running, &["first", "last"]),
        ["idle", "ranked last", "ranked first", "interacted", "light"]
    );
}

#[test]
fn within_a_space_the_one_idle_longest_goes_first_else_the_newest() {
    let mut idle_long = agent("idle long", "s");
    idle_long.idle_since = Some(at(1));
    let mut idle_short = agent("idle short", "s");
    idle_short.idle_since = Some(at(9));
    let mut old = agent("old", "s");
    old.running_for = Duration::from_millis(1500);
    let mut new = agent("new", "s");
    // Started within the same second.
    new.running_for = Duration::from_millis(1200);
    let running = Running {
        agents: vec![idle_short, old, idle_long, new],
        ..Running::default()
    };
    assert_eq!(
        order(&running, &["s"]),
        ["idle long", "idle short", "new", "old"]
    );
}

#[test]
fn stopping_an_agent_stops_the_agents_below_it_first() {
    let running = Running {
        jobs: vec![RunningJob {
            space: "job".into(),
            started: at(1),
        }],
        agents: vec![
            agent("coordinator", "top"),
            agent("child", "middle"),
            agent("grandchild", "bottom"),
            agent("other", "elsewhere"),
        ],
        parents: HashMap::from([
            ("middle".into(), "top".into()),
            ("bottom".into(), "middle".into()),
        ]),
        ..Running::default()
    };
    let order = StopOrder::new(&running, &[]);
    let coordinator = order
        .stops()
        .iter()
        .find(|stop| stop.agent().is_some_and(|id| id.as_str() == "coordinator"))
        .unwrap();
    assert_eq!(
        names(order.with_agents_below(coordinator).into_iter()),
        ["grandchild", "child", "coordinator"]
    );
    let job = &order.stops()[0];
    assert_eq!(names(order.with_agents_below(job).into_iter()), ["job job"]);
}

#[test]
fn a_cycle_in_the_tree_leaves_no_agent_out() {
    let running = Running {
        agents: vec![agent("a", "a"), agent("b", "b")],
        parents: HashMap::from([("a".into(), "b".into()), ("b".into(), "a".into())]),
        ..Running::default()
    };
    assert_eq!(order(&running, &[]).len(), 2);
}

#[test]
fn no_agent_above_an_unreachable_one_is_in_the_order() {
    let running = Running {
        agents: vec![
            agent("coordinator", "top"),
            agent("child", "middle"),
            agent("other", "elsewhere"),
        ],
        parents: HashMap::from([
            ("middle".into(), "top".into()),
            ("bottom".into(), "middle".into()),
        ]),
        unreachable: HashSet::from(["bottom".into()]),
        ..Running::default()
    };
    assert_eq!(order(&running, &[]), ["other"]);
}
