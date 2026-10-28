use super::*;
use anyhow::Context as _;
use tempfile::TempDir;

fn place(socket: &str, workspace: &str, demo: Option<&str>) -> Place {
    Place {
        socket: socket.into(),
        pane: "w1:p1".into(),
        terminal: "original-terminal".into(),
        ownership: uuid::Uuid::new_v4().to_string(),
        workspace: workspace.into(),
        demo: demo.map(Into::into),
    }
}

#[tokio::test]
async fn ownership_is_server_and_state_scoped_and_relabelling_replaces_it() -> Result<()> {
    let state = TempDir::new()?;
    let other_state = TempDir::new()?;
    let _lock = records::lock(state.path()).await?;
    let first = place("/server-one", "workspace", Some("old-demo"));
    let second = place("/server-two", "other-workspace", None);
    records::write(state.path(), &first)?;
    records::write(state.path(), &second)?;
    assert_ne!(first.key(), second.key());
    assert_eq!(Herdr.sessions(state.path(), false).await?.len(), 2);
    assert!(Herdr.sessions(other_state.path(), true).await?.is_empty());
    let replacement = place("/server-one", "new-workspace", None);
    records::write(state.path(), &replacement)?;
    let current = records::read(state.path(), &first.key())?.context("missing ownership")?;
    assert!(!current.owned_by(&Owner::Workspace("workspace".into())));
    assert!(!current.owned_by(&Owner::Demo("old-demo".into())));
    assert!(current.owned_by(&Owner::Workspace("new-workspace".into())));
    records::forget(state.path(), &first.key())?;
    assert_eq!(Herdr.sessions(state.path(), false).await?.len(), 1);
    Ok(())
}

#[test]
fn ownership_keys_cannot_escape_the_state_directory() {
    assert!(records::read(Path::new("/unused"), "../outside").is_err());
}
