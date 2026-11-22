use super::*;
use crate::test_daemon::{Connect as _, code};
use shoal_daemon::test_daemon::TestDaemon;
use shoal_protocol::{model::Workspace, protocol::ErrorCode};

/// Bytes longer than one frame carries, none of them alike in a row.
fn long_bytes() -> Vec<u8> {
    (0..3 * shoal_protocol::protocol::MAX_FRAME)
        .map(|n| (n % 251) as u8)
        .collect()
}

/// Attach a file holding `bytes` to `workspace` as the user; its record.
async fn attached(daemon: &TestDaemon, workspace: &Workspace, bytes: &[u8]) -> AttachedFile {
    let path = daemon.manager.paths.home.join("chart.bin");
    std::fs::write(&path, bytes).unwrap();
    let space = SpaceSelector::id(&workspace.id);
    let added = add(space, vec![path.into()])
        .send(&daemon.connector())
        .await;
    added.unwrap().remove(0)
}

#[tokio::test]
async fn a_read_brings_an_attached_files_bytes_across_frames() {
    let daemon = TestDaemon::start().await;
    let workspace = daemon.workspace("charts").await;
    let bytes = long_bytes();
    let file = attached(&daemon, &workspace, &bytes).await;
    let space = SpaceSelector::id(&workspace.id);
    let read = read(&daemon.connector(), space, file.id).await.unwrap();
    assert_eq!(read.to_end().await.unwrap(), bytes);
}

#[tokio::test]
async fn an_agent_reads_its_own_spaces_files_and_not_anothers() {
    let daemon = TestDaemon::start().await;
    let (own, other) = (
        daemon.workspace("own").await,
        daemon.workspace("other").await,
    );
    let file = attached(&daemon, &other, b"plan").await;
    let theirs = SpaceSelector::id(&other.id);
    let agent = daemon.scoped(&own).await;
    let refused = read(&agent, theirs.clone(), file.id).await.map(drop);
    assert_eq!(code(refused), ErrorCode::ScopeDenied);
    let owner = daemon.scoped(&other).await;
    let read = read(&owner, theirs, file.id).await.unwrap();
    assert_eq!(read.to_end().await.unwrap(), b"plan");
}
