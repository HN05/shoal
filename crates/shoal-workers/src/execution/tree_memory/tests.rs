use super::*;

fn write_kills(group: &Path, kills: u64) {
    let events = format!("oom 1\noom_kill {kills}\n");
    std::fs::write(group.join("memory.events"), events).unwrap();
}

#[tokio::test]
async fn a_command_counts_the_kills_in_its_control_group_while_it_ran() {
    let group = tempfile::tempdir().unwrap();
    write_kills(group.path(), 2);
    let (using, _) = watch::channel(0);
    let watch = Watch::start_in(std::process::id(), Some(group.path().to_owned()), using);
    write_kills(group.path(), 3);
    assert_eq!(watch.finish().oom_kills, 1);
}

#[tokio::test]
async fn a_command_in_no_readable_control_group_counts_no_kills() {
    let group = tempfile::tempdir().unwrap();
    let (using, _) = watch::channel(0);
    let watch = Watch::start_in(std::process::id(), Some(group.path().to_owned()), using);
    write_kills(group.path(), 3);
    assert_eq!(watch.finish().oom_kills, 0);
}
