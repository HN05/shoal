use super::*;
use shoal_protocol::launches::SpaceChoice;

/// A launch with `target` as its positional argument, as `change` sets the rest.
fn launch(target: Option<&str>, change: impl FnOnce(&mut Launch)) -> Launch {
    let mut launch = Launch {
        target: target.map(|target| target.parse().unwrap()),
        ..Default::default()
    };
    change(&mut launch);
    launch
}

/// A launch in the existing workspace 3, as `change` sets the rest.
fn in_workspace(change: impl FnOnce(&mut Launch)) -> Launch {
    launch(None, |launch| {
        launch.in_workspace = Some(SpaceChoice::Space("3".parse().unwrap()));
        change(launch);
    })
}

fn issue(number: &str) -> Option<IssueChoice> {
    Some(IssueChoice::Named(number.parse().unwrap()))
}

#[test]
fn a_start_refuses_flags_its_kind_of_space_cannot_use() {
    let chat = |launch: Launch, unfit: &[&'static str]| {
        ensure_fits(&launch, unfit, SpaceKind::Chat, Some(&"plan".into()))
    };
    let talk = launch(Some("repo"), |launch| {
        launch.issue = issue("7");
        launch.name = Some("talk".into());
    });
    chat(talk, &[]).unwrap();
    // A chat's agent waits for a slot as a workspace's does, so it takes --now.
    chat(launch(Some("repo"), |launch| launch.now = true), &[]).unwrap();
    let unfit = [
        (
            launch(Some("repo"), |l| l.branch = Some("topic".into())),
            "--branch",
        ),
        (
            launch(Some("repo"), |l| l.base = Some("main".into())),
            "--base",
        ),
        (
            launch(Some("repo"), |l| l.path = Some("/work/x".into())),
            "--path",
        ),
        (
            launch(Some("repo"), |l| l.skip_setup = true),
            "--skip-setup",
        ),
    ];
    for (launch, flag) in unfit {
        // The refusal names the flag and the kind of space.
        let error = chat(launch, &[]).unwrap_err().to_string();
        assert!(
            error.starts_with(&format!("{flag} ")) && error.contains("chat"),
            "{error}"
        );
    }
    // A flag only the caller reads is refused the same way.
    let error = chat(launch(Some("repo"), |_| ()), &["--focus"]).unwrap_err();
    assert!(error.to_string().starts_with("--focus "), "{error}");
    let workspace =
        |launch: Launch| ensure_fits(&launch, &[], SpaceKind::Workspace, Some(&"worker".into()));
    workspace(launch(Some("repo"), |launch| {
        launch.branch = Some("topic".into());
        launch.base = Some("main".into());
    }))
    .unwrap();
    workspace(launch(Some("repo"), |l| {
        l.name = Some("Login retries".into())
    }))
    .unwrap();
}

#[test]
fn a_workspaces_worker_refuses_what_only_a_new_space_takes() {
    let worker =
        |launch: Launch| ensure_fits(&launch, &[], SpaceKind::Workspace, Some(&"worker".into()));
    worker(in_workspace(|launch| {
        launch.blockers = vec!["12".into()];
        launch.fork = Some("session".into());
    }))
    .unwrap();
    worker(in_workspace(|launch| launch.now = true)).unwrap();
    // The issue's refusal names the command that links it to the workspace.
    let error = worker(in_workspace(|launch| launch.issue = issue("7")))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("shoal link issue 7 assigned --space 3"),
        "{error}"
    );
    let new_space = [
        (
            in_workspace(|l| l.parent = Some("2".parse().unwrap())),
            "--parent",
        ),
        (in_workspace(|l| l.name = Some("Login".into())), "--name"),
        (
            in_workspace(|l| l.priority = Some("urgent".parse().unwrap())),
            "--priority",
        ),
        (
            in_workspace(|l| l.target = Some("repo".parse().unwrap())),
            "<repository>",
        ),
    ];
    for (launch, flag) in new_space {
        let error = worker(launch).unwrap_err().to_string();
        assert!(error.starts_with(&format!("{flag} ")), "{error}");
    }
    // A chat in the worktree may take its own issue and name.
    let reader = in_workspace(|launch| {
        launch.issue = issue("7");
        launch.name = Some("Review".into());
        launch.priority = Some("low".parse().unwrap());
    });
    ensure_fits(&reader, &[], SpaceKind::Chat, Some(&"chat".into())).unwrap();
}

#[test]
fn a_start_in_a_workspace_refuses_what_no_role_takes_before_its_picker() {
    let current = |change: fn(&mut Launch)| {
        launch(None, |launch| {
            launch.in_workspace = Some(SpaceChoice::Current);
            change(launch);
        })
    };
    ensure_in_fits(&current(|launch| {
        launch.mode = Some(ModeName::CHAT);
        launch.issue = issue("7");
    }))
    .unwrap();
    // The worker mode starts the workspace's own agent, so what only a new
    // space takes is refused before the picker too.
    ensure_in_fits(&current(|launch| launch.mode = Some(ModeName::DEFAULT))).unwrap();
    let refused: [fn(&mut Launch); 5] = [
        |launch| launch.mode = Some(ModeName::SETUP),
        |launch| launch.target = Some("repo".parse().unwrap()),
        |launch| launch.repository = Some("repo".into()),
        |launch| {
            launch.mode = Some(ModeName::DEFAULT);
            launch.name = Some("Other".into());
        },
        |launch| {
            launch.mode = Some(ModeName::DEFAULT);
            launch.issue = issue("7");
        },
    ];
    for change in refused {
        let launch = current(change);
        assert!(ensure_in_fits(&launch).is_err(), "{launch:?}");
    }
    // Without --in, the start's own checks decide.
    ensure_in_fits(&launch(Some("repo"), |l| l.mode = Some(ModeName::SETUP))).unwrap();
}

#[test]
fn a_chat_takes_a_positional_issue_as_its_issue() {
    let target = |positional: &str, repository: Option<&str>, issue: Option<IssueChoice>| {
        chat_target(
            Some(positional.to_owned().parse().unwrap()),
            repository.map(str::to_owned),
            issue,
            false,
        )
    };
    let url = "https://forge.example/team/repo/issues/37";
    assert_eq!(
        target("37", Some("repo"), None).unwrap(),
        (Some("repo".into()), issue("37"))
    );
    assert_eq!(target(url, None, None).unwrap(), (None, issue(url)));
    assert_eq!(
        target("repo", None, Some(IssueChoice::Pick)).unwrap(),
        (Some("repo".into()), Some(IssueChoice::Pick))
    );
    for (positional, repository, named, refusal) in [
        ("37", None, issue("38"), "name the issue once"),
        (
            "repo",
            Some("other"),
            None,
            "cannot be combined with --repo",
        ),
        (
            "https://forge.example/team/repo/pulls/4",
            None,
            None,
            "a chat starts on an issue",
        ),
    ] {
        let error = target(positional, repository, named)
            .unwrap_err()
            .to_string();
        assert!(error.contains(refusal), "{positional}: {error}");
    }
}

#[test]
fn a_chat_in_a_workspace_takes_no_repository() {
    let in_workspace = |positional: Option<&str>, repository: Option<&str>| {
        chat_target(
            positional.map(|positional| positional.parse().unwrap()),
            repository.map(str::to_owned),
            None,
            true,
        )
    };
    assert_eq!(in_workspace(Some("37"), None).unwrap(), (None, issue("37")));
    for (positional, repository) in [(None, Some("other")), (Some("other"), None)] {
        let error = in_workspace(positional, repository)
            .unwrap_err()
            .to_string();
        assert!(error.contains("--in"), "{error}");
    }
}

#[test]
fn a_copy_refuses_a_pr_or_branch_link_but_takes_an_issue() {
    let copy = |target: &str| {
        launch(Some(target), |launch| {
            launch.copy = Some(SpaceChoice::Space("4".parse().unwrap()));
        })
    };
    let fits = |launch: Launch| ensure_copy_fits(&launch).is_ok();
    assert!(fits(copy("repo")));
    assert!(fits(launch(
        Some("https://forge.example/o/r/issues/7"),
        |launch| {
            launch.copy = Some(SpaceChoice::Current);
        }
    )));
    assert!(!fits(copy("https://forge.example/o/r/pulls/8")));
    let branch = "https://forge.example/o/r/src/branch/feature";
    assert!(!fits(launch(Some(branch), |launch| {
        launch.checkpoint = Some("2".parse().unwrap());
    })));
    assert!(fits(launch(
        Some("https://forge.example/o/r/pulls/8"),
        |_| ()
    )));
}

#[test]
fn the_arguments_other_than_the_mode_decide_the_kind_they_fit() {
    let fits = |target: &str, change: fn(&mut Launch)| fitting_kind(&launch(Some(target), change));
    assert_eq!(fits("repo", |_| ()), None);
    assert_eq!(fits("https://forge.example/o/r/issues/7", |_| ()), None);
    let workspace = Some(SpaceKind::Workspace);
    assert_eq!(fits("repo", |l| l.branch = Some("topic".into())), workspace);
    assert_eq!(fits("repo", |l| l.base = Some("main".into())), workspace);
    assert_eq!(fits("https://forge.example/o/r/pulls/8", |_| ()), workspace);
    // A name and --now fit either kind.
    assert_eq!(fits("repo", |l| l.name = Some("talk".into())), None);
    assert_eq!(fits("repo", |l| l.now = true), None);
}

#[test]
fn an_issues_labels_name_its_mode() {
    let config = config::Issue {
        required_labels: Vec::new(),
        plan_label: Some("needs-plan".into()),
        planned_label: Some("planned".into()),
        assign: Default::default(),
        area_prefix: None,
    };
    let mode = |labels: &[&str]| {
        let labels: Vec<String> = labels.iter().map(|label| (*label).into()).collect();
        labelled_mode(&config, &labels)
    };
    assert_eq!(mode(&["type/bug", "planned"]), Some(ModeName::DEFAULT));
    assert_eq!(mode(&["needs-plan"]), Some(ModeName::PLAN));
    // A settled plan wins over a stale label asking for one.
    assert_eq!(mode(&["needs-plan", "planned"]), Some(ModeName::DEFAULT));
    assert_eq!(mode(&["type/bug"]), None);
    let unset = config::Issue {
        plan_label: None,
        planned_label: None,
        ..config
    };
    assert_eq!(labelled_mode(&unset, &["needs-plan".into()]), None);
}

#[test]
fn a_start_names_an_issue_with_issue_or_positionally() {
    let named = |launch: Launch| named_issue(&launch).unwrap().map(|issue| issue.to_string());
    let url = "https://forge.example/o/r/issues/7";
    let with_issue = launch(Some("repo"), |launch| launch.issue = issue("7"));
    assert_eq!(named(with_issue).as_deref(), Some("7"));
    assert_eq!(named(launch(Some("7"), |_| ())).as_deref(), Some("7"));
    assert_eq!(named(launch(Some(url), |_| ())).as_deref(), Some(url));
    assert_eq!(named(launch(Some("repo"), |_| ())), None);
    assert_eq!(
        named(launch(Some("https://forge.example/o/r/pulls/8"), |_| ())),
        None
    );
    let picked = launch(Some("repo"), |launch| {
        launch.issue = Some(IssueChoice::Pick)
    });
    assert_eq!(named(picked), None);
}

#[test]
fn a_numeric_positional_may_name_the_repository_whose_modes_apply() {
    let positional = |launch: Launch| positional_repository(&launch).map(str::to_owned);
    let branch = launch(Some("007"), |launch| launch.branch = Some("topic".into()));
    assert_eq!(positional(branch).as_deref(), Some("007"));
    assert_eq!(
        positional(launch(Some("repo"), |_| ())).as_deref(),
        Some("repo")
    );
    assert_eq!(
        positional(launch(Some("https://forge.example/o/r/issues/7"), |_| ())),
        None
    );
}
