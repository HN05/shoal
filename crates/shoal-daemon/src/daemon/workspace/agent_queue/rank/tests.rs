use super::*;

const NOW: i64 = 100_000;

fn at(seconds: i64) -> UnixTime {
    UnixTime::from_seconds(seconds)
}

fn space(snapshot: &mut Snapshot, id: &str, tier: Tier, created: i64) {
    snapshot.spaces.insert(
        id.into(),
        Space {
            label: format!("#{id}"),
            priority: None,
            tier,
            steps: Steps::default(),
            created_at: at(created),
        },
    );
}

fn entry(snapshot: &mut Snapshot, id: &str, work: Work) {
    let arrival = snapshot.entries.len();
    snapshot.entries.push(Entry {
        id: id.into(),
        name: id.into(),
        priority: None,
        work,
        queued_at: at(NOW),
        arrival,
    });
}

fn edge(snapshot: &mut Snapshot, waiting: &str, on: &str, dependency: Dependency) {
    snapshot.edges.push(Edge {
        waiting: Node::Space(waiting.into()),
        on: Node::Space(on.into()),
        dependency,
        carries: Carries::Rank,
    });
}

fn restore(space: &str) -> Work {
    Work::Restore {
        space: space.into(),
    }
}

fn launch(space: Option<&str>) -> Work {
    Work::Launch {
        space: space.map(Into::into),
    }
}

fn snapshot() -> Snapshot {
    Snapshot {
        now: at(NOW),
        ..Snapshot::default()
    }
}

fn order(snapshot: &Snapshot) -> Vec<String> {
    rank(snapshot)
        .into_iter()
        .map(|ranked| ranked.id.to_string())
        .collect()
}

fn rank_of(snapshot: &Snapshot, id: &str) -> QueueRank {
    rank(snapshot)
        .into_iter()
        .find(|ranked| ranked.id.as_str() == id)
        .unwrap()
        .rank
}

/// A rank of the default priority at `tier`, taken from `tier_from`.
fn tiered(tier: Tier, tier_from: Option<InheritedRank>) -> QueueRank {
    QueueRank {
        priority: Priority::NORMAL,
        priority_from: None,
        tier,
        tier_from,
    }
}

/// Set `priority` on the space `id`.
fn prioritize(snapshot: &mut Snapshot, id: &str, priority: Priority) {
    snapshot
        .spaces
        .get_mut(&SpaceId::from(id))
        .unwrap()
        .priority = Some(priority);
}

fn inherited(from: &str, dependency: Dependency) -> Option<InheritedRank> {
    Some(InheritedRank {
        from: from.into(),
        dependency,
    })
}

#[test]
fn finishing_work_goes_before_started_work_before_new_work() {
    let mut snapshot = snapshot();
    space(&mut snapshot, "fresh", Tier::New, 1);
    space(&mut snapshot, "worked", Tier::Started, 2);
    space(&mut snapshot, "reviewed", Tier::Finishing, 3);
    entry(&mut snapshot, "launch", launch(None));
    entry(&mut snapshot, "into-fresh", launch(Some("fresh")));
    entry(&mut snapshot, "worked", restore("worked"));
    entry(&mut snapshot, "reviewed", restore("reviewed"));
    assert_eq!(
        order(&snapshot),
        ["reviewed", "worked", "into-fresh", "launch"]
    );
    assert_eq!(rank_of(&snapshot, "reviewed").tier, Tier::Finishing);
    assert_eq!(rank_of(&snapshot, "launch").tier, Tier::New);
}

#[test]
fn within_a_tier_steps_done_then_the_older_space_then_arrival_decide() {
    let mut snapshot = snapshot();
    for (id, created) in [("young", 5), ("old", 1), ("planned", 9), ("later", 1)] {
        space(&mut snapshot, id, Tier::Started, created);
    }
    let planned = snapshot.spaces.get_mut(&SpaceId::from("planned")).unwrap();
    planned.steps = Steps { done: 1, total: 3 };
    entry(&mut snapshot, "young", restore("young"));
    entry(&mut snapshot, "old", restore("old"));
    entry(&mut snapshot, "planned", restore("planned"));
    entry(&mut snapshot, "later", restore("later"));
    assert_eq!(order(&snapshot), ["planned", "old", "later", "young"]);
}

#[test]
fn a_larger_share_of_steps_done_goes_first_whatever_the_counts() {
    assert_eq!(
        Steps { done: 2, total: 3 }.cmp_share(Steps { done: 3, total: 5 }),
        Ordering::Greater
    );
    assert_eq!(
        Steps { done: 0, total: 0 }.cmp_share(Steps { done: 0, total: 4 }),
        Ordering::Equal
    );
}

#[test]
fn an_entry_rises_a_tier_for_each_full_hour_it_waits_up_to_finishing() {
    let mut snapshot = snapshot();
    for (id, waited) in [("hour", 3600), ("almost", 3599), ("day", 86_400)] {
        entry(&mut snapshot, id, launch(None));
        snapshot.entries.last_mut().unwrap().queued_at = at(NOW - waited);
    }
    assert_eq!(rank_of(&snapshot, "hour").tier, Tier::Started);
    assert_eq!(rank_of(&snapshot, "almost").tier, Tier::New);
    assert_eq!(rank_of(&snapshot, "day").tier, Tier::Finishing);
    assert_eq!(order(&snapshot), ["day", "hour", "almost"]);
}

#[test]
fn work_waited_on_takes_the_rank_of_what_waits_transitively() {
    let mut snapshot = snapshot();
    space(&mut snapshot, "parent", Tier::Finishing, 1);
    space(&mut snapshot, "child", Tier::Started, 2);
    space(&mut snapshot, "base", Tier::New, 3);
    space(&mut snapshot, "other", Tier::Started, 0);
    edge(&mut snapshot, "parent", "child", Dependency::Parent);
    edge(&mut snapshot, "child", "base", Dependency::Stacked);
    entry(&mut snapshot, "other", restore("other"));
    entry(&mut snapshot, "base", launch(Some("base")));
    assert_eq!(order(&snapshot), ["base", "other"]);
    assert_eq!(
        rank_of(&snapshot, "base"),
        tiered(Tier::Finishing, inherited("#child", Dependency::Stacked))
    );
}

#[test]
fn a_launch_without_a_space_passes_its_rank_to_what_blocks_it() {
    let mut snapshot = snapshot();
    space(&mut snapshot, "blocker", Tier::New, 1);
    entry(&mut snapshot, "blocked", launch(None));
    snapshot.entries[0].queued_at = at(NOW - 2 * 3600);
    entry(&mut snapshot, "blocker", launch(Some("blocker")));
    snapshot.edges.push(Edge {
        waiting: Node::Entry("blocked".into()),
        on: Node::Space("blocker".into()),
        dependency: Dependency::Blocked,
        carries: Carries::Rank,
    });
    assert_eq!(
        rank_of(&snapshot, "blocker"),
        tiered(Tier::Finishing, inherited("blocked", Dependency::Blocked))
    );
}

#[test]
fn rank_flows_only_from_waiting_work_to_what_it_waits_on() {
    let mut snapshot = snapshot();
    space(&mut snapshot, "waiting", Tier::New, 1);
    space(&mut snapshot, "awaited", Tier::Finishing, 2);
    edge(&mut snapshot, "waiting", "awaited", Dependency::Blocked);
    entry(&mut snapshot, "waiting", launch(Some("waiting")));
    assert_eq!(rank_of(&snapshot, "waiting"), tiered(Tier::New, None));
}

#[test]
fn cycles_of_waiting_work_settle() {
    let mut snapshot = snapshot();
    space(&mut snapshot, "a", Tier::Finishing, 1);
    space(&mut snapshot, "b", Tier::New, 2);
    edge(&mut snapshot, "a", "b", Dependency::Blocked);
    edge(&mut snapshot, "b", "a", Dependency::Blocked);
    entry(&mut snapshot, "b", launch(Some("b")));
    assert_eq!(rank_of(&snapshot, "b").tier, Tier::Finishing);
}

#[test]
fn a_job_ranks_with_the_highest_space_it_serves_and_goes_ahead_of_its_agent() {
    let mut snapshot = snapshot();
    space(&mut snapshot, "low", Tier::New, 1);
    space(&mut snapshot, "high", Tier::Started, 2);
    space(&mut snapshot, "older", Tier::Started, 0);
    entry(&mut snapshot, "high", restore("high"));
    entry(&mut snapshot, "older", restore("older"));
    entry(
        &mut snapshot,
        "job",
        Work::Job {
            tier: Tier::New,
            spaces: vec!["low".into(), "high".into()],
        },
    );
    entry(
        &mut snapshot,
        "alone",
        Work::Job {
            tier: Tier::New,
            spaces: Vec::new(),
        },
    );
    assert_eq!(order(&snapshot), ["older", "job", "high", "alone"]);
    assert_eq!(
        rank_of(&snapshot, "job"),
        tiered(Tier::Started, inherited("#high", Dependency::Serves))
    );
}

#[test]
fn a_job_on_an_open_pr_keeps_its_own_tier_above_its_spaces() {
    let mut snapshot = snapshot();
    space(&mut snapshot, "fresh", Tier::New, 1);
    entry(
        &mut snapshot,
        "job",
        Work::Job {
            tier: Tier::Finishing,
            spaces: vec!["fresh".into()],
        },
    );
    assert_eq!(rank_of(&snapshot, "job"), tiered(Tier::Finishing, None));
}

#[test]
fn a_review_ranks_with_its_workspace_and_what_that_inherits() {
    let mut snapshot = snapshot();
    space(&mut snapshot, "parent", Tier::Finishing, 1);
    space(&mut snapshot, "child", Tier::New, 2);
    edge(&mut snapshot, "parent", "child", Dependency::Parent);
    entry(&mut snapshot, "launch", launch(None));
    entry(
        &mut snapshot,
        "review",
        Work::Review {
            space: Some("child".into()),
        },
    );
    assert_eq!(order(&snapshot), ["review", "launch"]);
    assert_eq!(
        rank_of(&snapshot, "review"),
        tiered(Tier::Finishing, inherited("#child", Dependency::Serves))
    );
}

#[test]
fn a_job_that_work_is_blocked_on_takes_that_works_rank() {
    let mut snapshot = snapshot();
    space(&mut snapshot, "blocked", Tier::Finishing, 1);
    entry(
        &mut snapshot,
        "main-job",
        Work::Job {
            tier: Tier::New,
            spaces: Vec::new(),
        },
    );
    snapshot.edges.push(Edge {
        waiting: Node::Space("blocked".into()),
        on: Node::Entry("main-job".into()),
        dependency: Dependency::Blocked,
        carries: Carries::Rank,
    });
    assert_eq!(
        rank_of(&snapshot, "main-job"),
        tiered(Tier::Finishing, inherited("#blocked", Dependency::Blocked))
    );
}

#[test]
fn a_job_ranks_with_the_space_that_orders_first_among_equal_tiers() {
    let mut snapshot = snapshot();
    space(&mut snapshot, "workspace", Tier::Started, 1);
    space(&mut snapshot, "chat", Tier::Started, 2);
    let chat = snapshot.spaces.get_mut(&SpaceId::from("chat")).unwrap();
    chat.steps = Steps { done: 2, total: 3 };
    entry(&mut snapshot, "chat", restore("chat"));
    entry(
        &mut snapshot,
        "job",
        Work::Job {
            tier: Tier::New,
            spaces: vec!["workspace".into(), "chat".into()],
        },
    );
    assert_eq!(order(&snapshot), ["job", "chat"]);
    assert_eq!(
        rank_of(&snapshot, "job").tier_from,
        inherited("#chat", Dependency::Serves)
    );
}

#[test]
fn an_hour_raises_work_from_the_tier_of_its_space() {
    let mut snapshot = snapshot();
    space(&mut snapshot, "worked", Tier::Started, 1);
    entry(
        &mut snapshot,
        "review",
        Work::Review {
            space: Some("worked".into()),
        },
    );
    entry(&mut snapshot, "launch", launch(Some("worked")));
    for entry in &mut snapshot.entries {
        entry.queued_at = at(NOW - 3600);
    }
    assert_eq!(
        rank_of(&snapshot, "review"),
        tiered(Tier::Finishing, inherited("#worked", Dependency::Serves))
    );
    assert_eq!(rank_of(&snapshot, "launch"), tiered(Tier::Finishing, None));
}

#[test]
fn priority_goes_before_tier_and_a_launch_keeps_the_one_it_was_queued_with() {
    let mut snapshot = snapshot();
    space(&mut snapshot, "reviewed", Tier::Finishing, 1);
    space(&mut snapshot, "fresh", Tier::New, 2);
    space(&mut snapshot, "unwanted", Tier::Finishing, 3);
    prioritize(&mut snapshot, "fresh", Priority::URGENT);
    prioritize(&mut snapshot, "unwanted", Priority::LOW);
    entry(&mut snapshot, "unwanted", restore("unwanted"));
    entry(&mut snapshot, "reviewed", restore("reviewed"));
    entry(&mut snapshot, "fresh", launch(Some("fresh")));
    entry(&mut snapshot, "new", launch(None));
    snapshot.entries[3].priority = Some(Priority::try_from(60).unwrap());
    assert_eq!(order(&snapshot), ["fresh", "new", "reviewed", "unwanted"]);
}

#[test]
fn priority_and_tier_flow_to_waited_on_work_each_from_where_it_is_highest() {
    let mut snapshot = snapshot();
    space(&mut snapshot, "urgent", Tier::New, 1);
    space(&mut snapshot, "reviewed", Tier::Finishing, 2);
    space(&mut snapshot, "base", Tier::New, 3);
    prioritize(&mut snapshot, "urgent", Priority::URGENT);
    edge(&mut snapshot, "urgent", "base", Dependency::Blocked);
    edge(&mut snapshot, "reviewed", "base", Dependency::Stacked);
    entry(&mut snapshot, "base", restore("base"));
    assert_eq!(
        rank_of(&snapshot, "base"),
        QueueRank {
            priority: Priority::URGENT,
            priority_from: inherited("#urgent", Dependency::Blocked),
            tier: Tier::Finishing,
            tier_from: inherited("#reviewed", Dependency::Stacked),
        }
    );
}

#[test]
fn a_chat_passes_its_children_only_a_priority_set_on_it() {
    let mut snapshot = snapshot();
    for (id, created) in [("chat", 1), ("set", 2), ("child", 3), ("other", 4)] {
        space(&mut snapshot, id, Tier::New, created);
    }
    snapshot
        .spaces
        .get_mut(&SpaceId::from("chat"))
        .unwrap()
        .tier = Tier::Finishing;
    prioritize(&mut snapshot, "set", Priority::URGENT);
    let urgent_child = Priority::try_from(70).unwrap();
    prioritize(&mut snapshot, "chat", urgent_child);
    for (parent, child) in [("chat", "child"), ("set", "other")] {
        snapshot.edges.push(Edge {
            waiting: Node::Space(parent.into()),
            on: Node::Space(child.into()),
            dependency: Dependency::Parent,
            carries: Carries::SetPriority,
        });
    }
    // The chat's set priority is passed on, its tier and what it inherits not.
    edge(&mut snapshot, "set", "chat", Dependency::Blocked);
    entry(&mut snapshot, "child", restore("child"));
    entry(&mut snapshot, "other", restore("other"));
    let child = rank_of(&snapshot, "child");
    assert_eq!((child.priority, child.tier), (urgent_child, Tier::Started));
    assert_eq!(child.priority_from, inherited("#chat", Dependency::Parent));
    assert_eq!(rank_of(&snapshot, "other").priority, Priority::URGENT);
}

#[test]
fn a_job_takes_the_priority_of_the_space_it_serves_even_when_lower() {
    let mut snapshot = snapshot();
    space(&mut snapshot, "unwanted", Tier::New, 1);
    prioritize(&mut snapshot, "unwanted", Priority::LOW);
    entry(
        &mut snapshot,
        "job",
        Work::Job {
            tier: Tier::New,
            spaces: vec!["unwanted".into()],
        },
    );
    entry(&mut snapshot, "launch", launch(None));
    assert_eq!(order(&snapshot), ["launch", "job"]);
    assert_eq!(
        rank_of(&snapshot, "job").priority_from,
        inherited("#unwanted", Dependency::Serves)
    );
}

#[test]
fn a_job_its_lower_priority_space_waits_on_keeps_that_priority() {
    let mut snapshot = snapshot();
    space(&mut snapshot, "unwanted", Tier::New, 1);
    prioritize(&mut snapshot, "unwanted", Priority::LOW);
    snapshot.edges.push(Edge {
        waiting: Node::Space("unwanted".into()),
        on: Node::Entry("job".into()),
        dependency: Dependency::Blocked,
        carries: Carries::Rank,
    });
    entry(
        &mut snapshot,
        "job",
        Work::Job {
            tier: Tier::New,
            spaces: vec!["unwanted".into()],
        },
    );
    entry(&mut snapshot, "launch", launch(None));
    assert_eq!(order(&snapshot), ["launch", "job"]);
    assert_eq!(rank_of(&snapshot, "job").priority, Priority::LOW);
}
