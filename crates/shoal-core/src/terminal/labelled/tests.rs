use super::*;
use crate::{
    sessions::HostName,
    terminal::{Host, Places, Start},
};
use futures_util::future::BoxFuture;
use std::sync::Mutex;
use tempfile::TempDir;

/// Whether another process could take the lock at `path` now.
fn free(path: &Path) -> Result<bool> {
    let file = OpenOptions::new().write(true).open(path)?;
    Ok(fs2::FileExt::try_lock_exclusive(&file).is_ok())
}

#[tokio::test]
async fn a_group_is_looked_for_once_at_a_time_per_host_and_label() -> Result<()> {
    let state = TempDir::new()?;
    let repo = lock_path(state.path(), "herdr", "repo");
    let held = lock(repo.clone()).await?;
    assert!(!free(&repo)?);

    // Other labels and other hosts' groups wait for none of it.
    let _others = (
        lock(lock_path(state.path(), "herdr", "other")).await?,
        lock(lock_path(state.path(), "rex", "repo")).await?,
    );
    drop(held);
    assert!(free(&repo)?);
    Ok(())
}

/// A host that records, as it looks for a group, whether another process
/// could take the lock at `lock` meanwhile.
struct Recorder {
    lock: PathBuf,
    free_while_looking: Mutex<Vec<bool>>,
}

impl Host for Recorder {
    fn name(&self) -> HostName {
        HostName::Herdr
    }

    fn serves(&self) -> bool {
        false
    }

    fn places(&self) -> Option<&dyn Places> {
        Some(self)
    }
}

impl Places for Recorder {
    fn open<'a>(&'a self, _start: &'a Start<'a>) -> BoxFuture<'a, Result<Origin>> {
        Box::pin(async { anyhow::bail!("the recorder opens no places") })
    }

    fn show<'a>(&'a self, _place: &'a Origin, _name: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("the recorder shows no places") })
    }

    fn background_places(&self) -> &'static str {
        "places"
    }

    fn background_place(&self, _name: &str) -> String {
        String::new()
    }

    fn group<'a>(
        &'a self,
        _near: Option<&'a Origin>,
        _label: &'a str,
        _cwd: &'a Path,
    ) -> BoxFuture<'a, Result<Option<Origin>>> {
        Box::pin(async move {
            self.free_while_looking
                .lock()
                .unwrap()
                .push(free(&self.lock)?);
            Ok(Some(Origin {
                place: None,
                group: Some("w1".into()),
                ..Origin::created(HostName::Herdr, String::new())
            }))
        })
    }
}

#[tokio::test]
async fn the_host_looks_for_and_creates_a_group_under_its_lock() -> Result<()> {
    let state = TempDir::new()?;
    let lock = lock_path(state.path(), "herdr", "repo");
    let recorder: &'static Recorder = Box::leak(Box::new(Recorder {
        lock: lock.clone(),
        free_while_looking: Mutex::default(),
    }));
    let settings = crate::config::Config::from_toml("")?
        .resolve(&crate::config::repo::ConfigLayers::default())?;
    let host = Configured {
        host: recorder,
        ..Configured::named(&settings, HostName::Herdr)
    };

    let group = labelled_group(state.path(), &host, None, "repo", state.path()).await?;

    assert_eq!(group.and_then(|origin| origin.group), Some("w1".into()));
    assert_eq!(*recorder.free_while_looking.lock().unwrap(), [false]);
    assert!(free(&lock)?);
    Ok(())
}
