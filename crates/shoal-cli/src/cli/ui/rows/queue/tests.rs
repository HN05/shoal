use super::*;
use crate::time::UnixTime;
use shoal_schedule::priority::Priority;

#[test]
fn queue_status_names_the_work_a_launch_waits_for() {
    let mut entry = QueueEntry {
        narrowing: Default::default(),
        mode: None,
        id: "entry".into(),
        kind: QueueKind::Launch,
        repository_id: Some("repo".into()),
        name: "ui".into(),
        agent: "claude".into(),
        issue_title: None,
        state: crate::state::QueueState::Queued,
        position: Some(2),
        created_at: UnixTime::from_seconds(0),
        space_id: None,
        number: None,
        execution: None,
        opens: None,
        queued_by: None,
        blockers: Vec::new(),
        waits_for_update: false,
        base: None,
        job: None,
        held_by: None,
        rank: None,
    };
    assert_eq!(queue_status_cell(&entry).0, "queued, position 2");
    entry.blockers = vec!["api".into(), "https://forge.example/o/r/issues/7".into()];
    assert_eq!(
        queue_status_cell(&entry).0,
        "queued, position 2, blocked by api, https://forge.example/o/r/issues/7"
    );
    entry.blockers.clear();
    entry.mode = Some("review".into());
    let plain = Palette::stdout(true);
    assert_eq!(
        queue_rows(std::slice::from_ref(&entry), &[], plain),
        ["○ ui  review  queued, position 2"]
    );
    entry.mode = None;
    entry.rank = Some(QueueRank {
        priority: Priority::URGENT,
        priority_from: Some(InheritedRank {
            from: "7".into(),
            dependency: Dependency::Parent,
        }),
        tier: Tier::Finishing,
        tier_from: Some(InheritedRank {
            from: "12".into(),
            dependency: Dependency::Blocked,
        }),
        shared_areas: Vec::new(),
    });
    assert_eq!(
        queue_status_cell(&entry).0,
        "queued, position 2, priority urgent (from 7, its parent), finishing (from 12, blocked on this)"
    );
    let rank = entry.rank.as_mut().unwrap();
    rank.shared_areas = vec![
        SharedArea {
            area: "area/daemon".into(),
            spaces: vec!["12".into(), "15".into()],
        },
        SharedArea {
            area: "area/cli".into(),
            spaces: vec!["3".into()],
        },
    ];
    assert!(
        queue_status_cell(&entry)
            .0
            .ends_with("(from 12, blocked on this), sharing area/daemon: 12, 15; area/cli: 3"),
    );
    entry.rank = None;
    entry.held_by = Some(HeldBy::Memory {
        needed: 4 << 30,
        left: 3 << 29,
        reserved: Vec::new(),
    });
    assert_eq!(
        queue_status_cell(&entry).0,
        "queued, position 2, held by memory: needs 4.0 GiB, 1.5 GiB left"
    );
    entry.held_by = Some(HeldBy::Cpu {
        busy: 97,
        level: 95,
    });
    assert!(queue_status_cell(&entry).0.ends_with("97% busy, 95% level"));
}

#[test]
fn queue_status_names_a_restore_that_waits_for_an_update() {
    let mut entry = QueueEntry {
        narrowing: Default::default(),
        mode: None,
        id: "entry".into(),
        kind: QueueKind::Restore,
        repository_id: Some("repo".into()),
        name: "ui".into(),
        agent: "claude".into(),
        issue_title: None,
        state: crate::state::QueueState::Queued,
        position: Some(1),
        created_at: UnixTime::from_seconds(0),
        space_id: Some("ui".into()),
        number: None,
        execution: Some("execution".into()),
        opens: None,
        queued_by: None,
        blockers: Vec::new(),
        waits_for_update: true,
        base: None,
        job: None,
        held_by: None,
        rank: None,
    };
    assert_eq!(
        queue_status_cell(&entry).0,
        "queued, position 1, after an update"
    );
    entry.state = crate::state::QueueState::Starting;
    entry.position = None;
    assert!(!queue_status_cell(&entry).0.contains("update"));
}
