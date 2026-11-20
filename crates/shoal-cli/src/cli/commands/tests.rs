use super::*;
use crate::cli::args::{
    coordination::CoordinationCommand, internal::InternalCommand, items::ItemsCommand,
};
use anyhow::Context as _;
use std::{cell::Cell, future::Future, task::Poll, time::Duration};
use tokio::sync::{Semaphore, mpsc};

fn workspaces(count: usize) -> Vec<Workspace> {
    (0..count)
        .map(|index| {
            Workspace::new_record(
                "repo".into(),
                format!("w{index}").as_str().into(),
                "/unused".into(),
                crate::model::Head::Branch(index.to_string().into()),
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
        let index: usize = workspace.name[1..].parse().unwrap();
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
        if workspace.name == "w0" {
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
fn watch_refuses_a_timeout_its_client_deadline_would_expire() {
    let parse = |timeout: &str| {
        <Cli as clap::Parser>::try_parse_from(["shoal", "watch", "--timeout", timeout])
    };
    assert!(parse("60").is_ok());
    // The client's own deadline would otherwise turn an invalid timeout into
    // an expired wait.
    for invalid in ["0", "3601"] {
        assert!(parse(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn a_resumed_watch_names_its_space_or_an_earlier_releases_workspace() {
    let parse = |arguments: &[&str]| {
        let cli = <Cli as clap::Parser>::try_parse_from(
            ["shoal", "watch", "pr"].iter().chain(arguments),
        )?;
        let Command::Items(ItemsCommand::Watch {
            resume,
            items,
            handed_over,
            ..
        }) = cli.command.unwrap()
        else {
            panic!("expected watch command");
        };
        assert!(resume);
        Ok::<_, clap::Error>((items.space, handed_over))
    };
    assert!(parse(&["--resume"]).is_err());
    assert_eq!(
        parse(&["--resume", "--space", "5"]).unwrap(),
        (Some("5".into()), None)
    );
    // The wait of an earlier release hands itself over this way.
    assert_eq!(
        parse(&["--resume", "--workspace", "5"]).unwrap(),
        (None, Some("5".into()))
    );
    assert!(parse(&["--resume", "--workspace", "5", "--space", "other"]).is_err());
}

#[test]
fn an_earlier_releases_alert_follower_hands_over_with_every_flag_it_took() {
    let cli = <Cli as clap::Parser>::try_parse_from([
        "shoal",
        "alerts",
        "--follow",
        "--keep-unread",
        "--limit",
        "100",
        "--resume",
        "7",
    ])
    .unwrap();
    let Some(Command::Coordination(CoordinationCommand::Alerts {
        keep_unread,
        resume,
        ..
    })) = cli.command
    else {
        panic!("expected the earlier release's follow");
    };
    assert_eq!((keep_unread, resume), (true, Some(7)));
}

#[test]
fn an_earlier_releases_review_watch_hands_over_its_workspace_unchanged() {
    let cli = <Cli as clap::Parser>::try_parse_from([
        "shoal",
        "watch",
        "review",
        "7",
        "--workspace",
        "5",
    ])
    .unwrap();
    let Some(Command::Items(ItemsCommand::Watch {
        items,
        handed_over,
        resume,
        ..
    })) = cli.command
    else {
        panic!("expected watch command");
    };
    assert!(items.selects_review() && !resume);
    assert_eq!(handed_over, Some("5".into()));
}

#[test]
fn send_posts_in_a_thread_its_number_leads_and_else_sends_an_alert() {
    use crate::cli::commands::send::{Given, Sending, sending};
    let parse = |arguments: &[&str]| {
        let arguments = ["shoal", "send"].iter().chain(arguments);
        let cli = <Cli as clap::Parser>::try_parse_from(arguments).unwrap();
        let Some(Command::Coordination(CoordinationCommand::Send {
            args,
            body_file,
            in_chat,
            attach,
            post,
            space,
            all,
            repository,
        })) = cli.command
        else {
            panic!("expected send");
        };
        let given = Given {
            body_file: body_file.is_some(),
            in_chat,
            attached: !attach.is_empty(),
            recipients: space.is_some() || all || repository.is_some(),
        };
        sending(args, &post, given)
    };
    let post = |sending: Sending| match sending {
        Sending::Post { thread, text } => Some((thread.to_string(), text)),
        _ => None,
    };
    let thread =
        |number: &str, text: Option<&str>| Some((number.to_owned(), text.map(str::to_owned)));
    assert_eq!(
        post(parse(&["3.2", "SQLite"]).unwrap()),
        thread("3.2", Some("SQLite"))
    );
    assert_eq!(
        post(parse(&["2", "--choose", "1"]).unwrap()),
        thread("2", None)
    );
    assert_eq!(
        post(parse(&["2", "--body-file", "a.md"]).unwrap()),
        thread("2", None)
    );
    // One argument alone is an alert's text, even a number.
    assert!(matches!(parse(&["2"]).unwrap(), Sending::Alert(Some(text)) if text == "2"));
    assert!(matches!(
        parse(&["--body-file", "a.md"]).unwrap(),
        Sending::Alert(None)
    ));
    // A post's options need a thread, and a post takes no recipients.
    assert!(parse(&["--wait"]).is_err());
    assert!(parse(&["3.2", "SQLite", "--space", "4"]).is_err());
    assert!(matches!(
        parse(&["Rebase", "--space", "4"]).unwrap(),
        Sending::Alert(_)
    ));
    // From the chat, the turn's final message is a thread's post or else an
    // alert, which takes recipients and files.
    assert!(matches!(
        parse(&["3.2", "--in-chat"]).unwrap(),
        Sending::PostInChat(thread) if thread.to_string() == "3.2"
    ));
    for alert in [
        &["--in-chat"][..],
        &["--in-chat", "--space", "4", "--attach", "a.png"],
        &["--in-chat", "--repo"],
    ] {
        assert!(matches!(parse(alert).unwrap(), Sending::AlertInChat));
    }
    assert!(parse(&["3.2", "--in-chat", "--all"]).is_err());
    assert!(parse(&["3.2", "--in-chat", "--attach", "a.png"]).is_err());
    assert!(parse(&["Rebase", "--in-chat"]).is_err());
}

#[test]
fn send_refuses_options_that_do_not_combine() {
    let parse = |arguments: &[&str]| {
        let arguments = ["shoal", "send"].iter().chain(arguments);
        <Cli as clap::Parser>::try_parse_from(arguments).map(drop)
    };
    assert!(parse(&["3.2", "SQLite", "--space", "4", "--wait"]).is_err());
    assert!(parse(&["--in-chat", "--body-file", "a.md"]).is_err());
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
        parse(&["--space", "5", "--resume"]).unwrap(),
        (Some("5".into()), None)
    );
    // The wake hook of an earlier release hands its wait over this way.
    assert_eq!(
        parse(&["--workspace", "5", "--resume"]).unwrap(),
        (None, Some("5".into()))
    );
    assert!(parse(&["--workspace", "5"]).is_err());
}
