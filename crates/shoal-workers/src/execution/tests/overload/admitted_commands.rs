//! A `shoal run` command that protection stops runs again once readings
//! recover, and each of its runs estimates the next.
use super::*;
use shoal_protocol::{
    events::SpaceEvent,
    log::{LogEntry, LogFilter},
    runs::CommandKey,
};

use crate::execution::CommandRun;

/// The command the rerun test runs: its first run counts itself and waits
/// to be stopped, its next counts itself and ends.
const COUNTED_RUNS: &str = "n=$(cat runs 2>/dev/null || echo 0); echo $((n + 1)) > runs; \
     trap 'exit 0' TERM; if [ \"$n\" = 0 ]; then while :; do sleep 1; done; fi";

#[tokio::test]
#[ignore = "admitted command launched by the rerun test"]
async fn admitted_command_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    let root = std::env::current_dir().unwrap();
    let paths = CommandPaths::from(crate::paths::Paths::for_test(root));
    let id = SpaceSelector::Id(std::env::var("SHOAL_TEST_WORKSPACE").unwrap().into());
    let run = CommandRun {
        key: CommandKey::Named("build".into()),
        now: false,
    };
    let command = vec!["sh".into(), "-c".into(), COUNTED_RUNS.into()];
    let code = crate::execution::run_command(&paths, id, command, Some(run))
        .await
        .unwrap();
    std::process::exit(code);
}

/// How many runs of the command have started in `workspace`.
fn runs(workspace: &crate::model::Workspace) -> u32 {
    let counted = fs::read_to_string(workspace.path.join("runs")).unwrap_or_default();
    counted.trim().parse().unwrap_or(0)
}

#[tokio::test]
async fn a_command_protection_stops_runs_again_once_readings_recover() {
    let (_root, manager, workspace, serving) = fixture().await;
    let command = launch_child(
        &manager,
        &workspace,
        "execution::tests::overload::admitted_commands::admitted_command_child",
    );
    let queue = manager.agents.agent_queue();
    let running = bounded(async {
        loop {
            if let [running] = queue.running_measured().as_slice()
                && runs(&workspace) == 1
            {
                break running.id.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(queue.stop_measured(&running, "critical memory pressure"));
    // The stopped run waits for healthy readings before it runs again.
    bounded(async {
        while queue.waiting_oneshots().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(runs(&workspace), 1);
    publish_recovery(&manager);
    assert_eq!(bounded(command).await.unwrap().unwrap(), 0);
    assert_eq!(runs(&workspace), 2);
    // The space's log keeps both runs, the stopped one cut short.
    assert_eq!(command_runs(&manager, &workspace).await, [false, true]);
    serving.abort();
}

#[tokio::test]
#[ignore = "command launched inside an admitted one by the nesting test"]
async fn nested_command_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    let root = std::env::current_dir().unwrap();
    let paths = CommandPaths::from(crate::paths::Paths::for_test(root));
    let id = SpaceSelector::Id(std::env::var("SHOAL_TEST_WORKSPACE").unwrap().into());
    let run = CommandRun {
        key: CommandKey::Named("nested".into()),
        now: false,
    };
    let command = vec!["true".into()];
    let code = crate::execution::run_command(&paths, id, command, Some(run))
        .await
        .unwrap();
    std::process::exit(code);
}

/// The `Command ran` entries of `workspace`'s log, by whether each ran
/// whole.
async fn command_runs(manager: &Manager, workspace: &crate::model::Workspace) -> Vec<bool> {
    let user = crate::client::connection::Connector {
        paths: manager.paths.clone(),
        scope: None,
    };
    let space = Some(SpaceSelector::id(&workspace.id));
    let log = crate::client::log::log(&user, space, LogFilter::Entries, 100);
    (log.await.unwrap().into_iter())
        .filter_map(|entry| match entry {
            LogEntry::Event(event) => match event.details.event {
                SpaceEvent::CommandRan { command } => Some(command.whole),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_command_inside_an_admitted_one_runs_without_its_own_admission() {
    let (_root, manager, workspace, serving) = fixture().await;
    let test = "execution::tests::overload::admitted_commands::nested_command_child";
    let mut command = child_command(&manager, &workspace, test);
    command.env(crate::env::ADMITTED_RUN, "1");
    assert_eq!(bounded(run_child(command)).await.unwrap().unwrap(), 0);
    assert!(command_runs(&manager, &workspace).await.is_empty());
    serving.abort();
}

#[tokio::test]
async fn a_signal_ends_a_stopped_command_that_waits_to_run_again() {
    let (_root, manager, workspace, serving) = fixture().await;
    let test = "execution::tests::overload::admitted_commands::admitted_command_child";
    let mut child = child_command(&manager, &workspace, test).spawn().unwrap();
    let queue = manager.agents.agent_queue();
    let running = bounded(async {
        loop {
            if let [running] = queue.running_measured().as_slice()
                && runs(&workspace) == 1
            {
                break running.id.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(queue.stop_measured(&running, "critical memory pressure"));
    bounded(async {
        while queue.waiting_oneshots().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    // SAFETY: kill takes no pointers; the PID is the child's, not yet reaped.
    unsafe { libc::kill(child.id().unwrap() as i32, libc::SIGTERM) };
    let status = bounded(child.wait()).await.unwrap();
    assert!(!status.success(), "{status:?}");
    assert_eq!(runs(&workspace), 1);
    serving.abort();
}
