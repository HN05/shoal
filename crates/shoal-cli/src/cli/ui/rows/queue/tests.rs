use super::*;
use crate::time::UnixTime;
use shoal_schedule::{priority::Priority, rank::QueueRank, rank::Tier};

/// A launch named `name` waiting at `position` in the space numbered
/// `number`.
fn launch(name: &str, position: usize, number: u32) -> QueueEntry {
    QueueEntry {
        narrowing: Default::default(),
        mode: None,
        id: format!("{name}-id").into(),
        kind: QueueKind::Launch,
        repository_id: Some("repo".into()),
        name: name.into(),
        agent: "claude".into(),
        issue_title: None,
        state: QueueState::Queued,
        position: Some(position),
        created_at: UnixTime::from_seconds(0),
        space_id: None,
        number: Some(number.into()),
        execution: None,
        opens: None,
        queued_by: None,
        blockers: Vec::new(),
        waits_for_update: false,
        base: None,
        job: None,
        held_by: None,
        rank: Some(QueueRank {
            priority: Priority::NORMAL,
            priority_from: None,
            tier: Tier::New,
            tier_from: None,
            shared_areas: Vec::new(),
        }),
    }
}

#[test]
fn the_queue_table_leads_with_each_place_and_space_number() {
    let mut starting = launch("fix-login", 0, 3);
    (starting.state, starting.position, starting.rank) = (QueueState::Starting, None, None);
    let mut urgent = launch("review-api", 1, 14);
    urgent.rank.as_mut().unwrap().priority = Priority::URGENT;
    urgent.held_by = Some(HeldBy::Memory {
        needed: 2 << 30,
        left: 0,
        reserved: Vec::new(),
    });
    let mut blocked = launch("docs", 12, 9);
    blocked.blockers = vec!["fix-login".into()];
    blocked.waits_for_update = true;
    blocked.held_by = Some(HeldBy::Cpu {
        busy: 97,
        level: 95,
    });
    let entries = [starting, urgent, blocked];
    let plain = Palette::stdout(true);
    let table = queue_rows(&entries, &[], plain);
    assert_eq!(table.header, " #  ID   NAME        PRIORITY  WAITS FOR");
    // Each row starts with its place, then its space's number, which `queue
    // start` takes.
    assert_eq!(
        table.rows,
        [
            "     3 ◌ fix-login             starting",
            " 1  14 ○ review-api  urgent    memory",
            "12   9 ○ docs        normal    fix-login, an update, CPU: 97% busy, 95% level",
        ]
    );
}

#[test]
fn a_restore_waits_for_an_update_only_while_queued() {
    let mut entry = launch("ui", 1, 2);
    entry.kind = QueueKind::Restore;
    entry.waits_for_update = true;
    assert_eq!(waits_for_cell(&entry).0, "an update");
    assert_eq!(waiting_reasons(&entry), ", after an update");
    entry.state = QueueState::Starting;
    assert_eq!(waits_for_cell(&entry).0, "starting");
    assert_eq!(waiting_reasons(&entry), "");
}
