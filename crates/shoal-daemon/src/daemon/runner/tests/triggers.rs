//! Triggers across the daemon's subsystems: a scoped caller's run, which
//! its `triggers` permission decides and the launch `shoal start` uses
//! starts.
use std::sync::Arc;

use shoal_protocol::triggers::{Fired, RunOrigin, RunOutcome};
use shoal_runner::daemon::Daemon;

use crate::{
    daemon::{
        approvals::Undecided,
        manager::Manager,
        scope::Caller,
        test_support::{chat, manager, repository},
    },
    model::Repository,
    protocol::ItemContext,
};

const HOURLY: &str = "[triggers.check]\non = 'timer'\nevery = '1h'\nmode = 'chat'\n\
                      agent = 'claude'\ntask = 'Check since {{ previous_at }}'\n";

/// A manager with a registered repository whose local config defines
/// `triggers`.
async fn with_triggers(triggers: &str) -> (tempfile::TempDir, Arc<Manager>, Repository) {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = (manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None))
    .await
    .unwrap();
    (manager.write_local_config(&repo.id, Some(triggers.into())))
        .await
        .unwrap();
    (temp, manager, repo)
}

/// How many runs the repository's trigger `name` recorded.
async fn run_count(manager: &Manager, repo: &Repository, name: &str) -> i64 {
    let (scope, name) = (repo.id.to_string(), name.to_owned());
    manager
        .store
        .read(move |tx| {
            let count = tx.query_row(
                "SELECT COUNT(*) FROM trigger_runs WHERE scope=?1 AND name=?2",
                rusqlite::params![scope, name],
                |row| row.get(0),
            )?;
            Ok(count)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn a_scoped_run_follows_the_triggers_permission() {
    let (_temp, manager, repo) = with_triggers(HOURLY).await;
    let agent = chat(&manager, "agent", "chat", Some(&repo), None).await;
    let caller = Caller {
        space_id: agent.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };
    let context = || Some(ItemContext::Checkout(repo.id.clone()));
    let daemon: Arc<dyn Daemon> = manager.clone();

    // The built-in modes propose it: an approval request, and no run.
    let error = (manager.runner)
        .run_trigger(&daemon, context(), "check", None, Some(&caller))
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<Undecided>().is_some(), "{error:#}");
    assert_eq!(run_count(&manager, &repo, "check").await, 0);

    let config = format!("{HOURLY}[permissions]\ntriggers = 'direct'\n");
    (manager.write_local_config(&repo.id, Some(config)))
        .await
        .unwrap();
    let fired = (manager.runner)
        .run_trigger(&daemon, context(), "check", None, Some(&caller))
        .await
        .unwrap();
    let Fired::Ran { run } = fired else {
        panic!("{fired:?}");
    };
    assert_eq!(
        (run.outcome, run.origin),
        (RunOutcome::Started, RunOrigin::Manual)
    );
    // The launch opened the chat the run names.
    let chats = manager.spaces.chats().await.unwrap();
    assert!(
        chats
            .iter()
            .any(|chat| Some(&chat.id) == run.space_id.as_ref())
    );
}
