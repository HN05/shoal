use super::*;
use crate::cli::args::{
    coordination::{CoordinationCommand, QuestionsCommand},
    internal::InternalCommand,
    items::ItemsCommand,
};
use anyhow::Context as _;
use std::{cell::Cell, future::Future, task::Poll, time::Duration};
use tokio::sync::{Semaphore, mpsc};

fn workspaces(count: usize) -> Vec<Workspace> {
    (0..count)
        .map(|index| {
            Workspace::new_record(
                "repo".into(),
                index.to_string(),
                "/unused".into(),
                index.to_string(),
                crate::state::WorkspaceState::Ready,
            )
        })
        .collect()
}

struct InFlight<'a>(&'a Cell<usize>);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

#[tokio::test]
async fn workspace_overviews_refill_past_delays_and_failures_in_workspace_order() {
    let limit = WORKSPACE_OVERVIEW_CONCURRENCY;
    let workspaces = workspaces(limit * 3);
    let gates: Vec<_> = workspaces.iter().map(|_| Semaphore::new(0)).collect();
    let active = Cell::new(0);
    let peak = Cell::new(0);
    let (started, mut starts) = mpsc::unbounded_channel();
    let collect = collect_workspace_overviews(&workspaces, async |workspace| {
        let index: usize = workspace.name.parse().unwrap();
        active.set(active.get() + 1);
        peak.set(peak.get().max(active.get()));
        let _active = InFlight(&active);
        started.send(index).unwrap();
        gates[index].acquire().await.unwrap().forget();
        if index == 1 {
            anyhow::bail!("controlled failure");
        }
        Ok(index)
    });
    let release = async {
        for index in 0..limit {
            assert_eq!(starts.recv().await, Some(index));
        }
        assert!(starts.try_recv().is_err());
        // Keep the first request pending while failures and later work free slots.
        for index in limit..workspaces.len() {
            gates[index - limit + 1].add_permits(1);
            assert_eq!(starts.recv().await, Some(index));
        }
        for gate in &gates {
            gate.add_permits(1);
        }
    };
    let (overviews, ()) = tokio::time::timeout(Duration::from_secs(60), async {
        tokio::join!(collect, release)
    })
    .await
    .expect("a delayed workspace blocked collection");
    assert_eq!(peak.get(), limit);
    assert_eq!(active.get(), 0);
    assert_eq!(overviews.len(), workspaces.len());
    for (index, result) in overviews.into_iter().enumerate() {
        match result {
            WorkspaceOverviewResult::Ready(value) => {
                assert_ne!(index, 1);
                assert_eq!(value, index);
            }
            WorkspaceOverviewResult::Failed { workspace, error } => {
                assert_eq!(index, 1);
                assert_eq!(workspace.id, workspaces[index].id);
                assert_eq!(error, "controlled failure");
            }
        }
    }
}

#[tokio::test]
async fn cancelling_workspace_overviews_drops_in_flight_requests() {
    let workspaces = workspaces(WORKSPACE_OVERVIEW_CONCURRENCY * 2);
    let active = Cell::new(0);
    let started = Cell::new(0);
    let mut collect = Box::pin(collect_workspace_overviews(&workspaces, async |_| {
        active.set(active.get() + 1);
        started.set(started.get() + 1);
        let _active = InFlight(&active);
        std::future::pending::<Result<()>>().await
    }));
    std::future::poll_fn(|cx| {
        assert!(collect.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(active.get(), WORKSPACE_OVERVIEW_CONCURRENCY);
    drop(collect);
    assert_eq!(active.get(), 0);
    assert_eq!(started.get(), WORKSPACE_OVERVIEW_CONCURRENCY);
}

#[tokio::test]
async fn empty_workspace_overviews_make_no_requests() {
    let overviews =
        collect_workspace_overviews::<()>(&[], async |_| panic!("empty collection made a request"))
            .await;
    assert!(overviews.is_empty());
}

#[tokio::test(start_paused = true)]
async fn workspace_overviews_account_for_request_timeouts() {
    let workspaces = workspaces(WORKSPACE_OVERVIEW_CONCURRENCY * 2);
    let overviews = collect_workspace_overviews(&workspaces, async |workspace| {
        if workspace.name == "0" {
            tokio::time::timeout(Duration::from_millis(1), std::future::pending::<()>())
                .await
                .context("daemon request timed out")?;
        }
        Ok(workspace.name.clone())
    })
    .await;
    assert_eq!(overviews.len(), workspaces.len());
    let WorkspaceOverviewResult::Failed { error, .. } = &overviews[0] else {
        panic!("timeout was not reported");
    };
    assert!(error.starts_with("daemon request timed out:"));
    assert!(overviews[1..].iter().all(|result| !result.is_failed()));
}

#[test]
fn bare_repo_opens_the_repository_menu() {
    let cli = <Cli as clap::Parser>::try_parse_from(["shoal", "repo"]).unwrap();
    assert_eq!(menu_path(cli.command.as_ref()), Some(&["repo"][..]));
    let cli = <Cli as clap::Parser>::try_parse_from(["shoal", "repo", "ls"]).unwrap();
    assert_eq!(menu_path(cli.command.as_ref()), None);
}

#[test]
fn watch_waits_without_a_default_timeout() {
    let cli = <Cli as clap::Parser>::try_parse_from(["shoal", "watch"]).unwrap();
    let Command::Items(ItemsCommand::Watch { timeout, .. }) = cli.command.unwrap() else {
        panic!("expected watch command");
    };
    assert_eq!(timeout, None);

    let cli = <Cli as clap::Parser>::try_parse_from(["shoal", "watch", "--timeout", "60"]).unwrap();
    let Command::Items(ItemsCommand::Watch { timeout, .. }) = cli.command.unwrap() else {
        panic!("expected watch command");
    };
    assert_eq!(timeout, Some(60));

    // The client's own deadline would otherwise turn an invalid timeout into
    // an expired wait.
    for invalid in ["0", "3601"] {
        assert!(
            <Cli as clap::Parser>::try_parse_from(["shoal", "watch", "--timeout", invalid])
                .is_err()
        );
    }
}

#[test]
fn a_resumed_watch_names_its_workspace() {
    let parse = |arguments: &[&str]| {
        <Cli as clap::Parser>::try_parse_from(["shoal", "watch", "pr"].iter().chain(arguments))
    };
    assert!(parse(&["--resume"]).is_err());
    let cli = parse(&["--resume", "--workspace", "work"]).unwrap();
    let Command::Items(ItemsCommand::Watch { resume, items, .. }) = cli.command.unwrap() else {
        panic!("expected watch command");
    };
    assert!(resume);
    assert_eq!(items.workspace.as_deref(), Some("work"));
}

#[test]
fn a_resumed_question_wait_names_its_space_or_an_earlier_releases_workspace() {
    let parse = |arguments: &[&str]| {
        let arguments = ["shoal", "questions", "ask", "Which?", "--recommend", "A"]
            .iter()
            .chain(arguments);
        let cli = <Cli as clap::Parser>::try_parse_from(arguments)?;
        let Some(Command::Coordination(CoordinationCommand::Questions {
            command: Some(QuestionsCommand::Ask(args)),
            ..
        })) = cli.command
        else {
            panic!("expected questions ask");
        };
        Ok::<_, clap::Error>((args.space, args.handed_over, args.resume))
    };
    assert_eq!(
        parse(&["--space", "work", "--resume", "3"]).unwrap(),
        (Some("work".to_owned()), None, Some(3))
    );
    // The wait of an earlier release hands itself over this way.
    assert_eq!(
        parse(&["--workspace", "work", "--resume", "3"]).unwrap(),
        (None, Some("work".to_owned()), Some(3))
    );
    assert!(parse(&["--workspace", "work"]).is_err());
    assert!(parse(&["--resume", "3"]).is_err());
}

#[test]
fn a_resumed_wake_names_its_space_or_an_earlier_releases_workspace() {
    let parse = |arguments: &[&str]| {
        let arguments = ["shoal", "internal", "wake", "--agent", "7"]
            .iter()
            .chain(arguments);
        let cli = <Cli as clap::Parser>::try_parse_from(arguments)?;
        let Some(Command::Internal {
            command:
                InternalCommand::Wake {
                    agent,
                    space,
                    handed_over,
                    resume,
                },
        }) = cli.command
        else {
            panic!("expected the wake hook");
        };
        assert_eq!(agent, 7);
        assert!(resume);
        Ok::<_, clap::Error>((space, handed_over))
    };
    assert_eq!(
        parse(&["--space", "work", "--resume"]).unwrap(),
        (Some("work".to_owned()), None)
    );
    // The wake hook of an earlier release hands its wait over this way.
    assert_eq!(
        parse(&["--workspace", "work", "--resume"]).unwrap(),
        (None, Some("work".to_owned()))
    );
    assert!(parse(&["--workspace", "work"]).is_err());
}
