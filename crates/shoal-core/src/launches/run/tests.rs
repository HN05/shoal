use super::*;
use crate::forge::{
    ci::JobState,
    numbers::{PullNumber, RunId},
};
use shoal_protocol::launches::SpaceChoice;

const RUN: &str = "https://forge.example/team/app/actions/runs/2765";

/// A run of `release.yml` on `branch`, for the PR `pull` if any.
fn run(branch: Option<&str>, pull: Option<u64>) -> Run {
    Run {
        id: RunId::new(9),
        url: RUN.into(),
        branch: branch.map(Into::into),
        workflow: Some("release.yml".into()),
        commit: None,
        pull: pull.map(PullNumber::new),
        state: JobState::Failed,
        result: None,
        waiting: false,
        finish: None,
    }
}

/// A launch on `RUN`, positionally or with `--ci`, as `change` sets the
/// rest.
fn launch(flagged: bool, change: impl FnOnce(&mut Launch)) -> Launch {
    let mut launch = Launch::default();
    match flagged {
        true => launch.ci = Some(ItemChoice::Named(RUN.parse().unwrap())),
        false => launch.target = Some(RUN.parse().unwrap()),
    }
    change(&mut launch);
    launch
}

#[test]
fn a_run_starts_where_it_ran() {
    let other = |read: &Run| RunStart::other_ref(read, "main").map(str::to_owned);
    let pull = run(Some("fix"), Some(12));
    assert_eq!(other(&pull), None);
    assert_eq!(
        RunStart::of(&pull, None),
        RunStart::Pull(PullNumber::new(12))
    );
    let branch = run(Some("fix"), None);
    assert_eq!(other(&branch).as_deref(), Some("fix"));
    let pushed = RunStart::of(&branch, other(&branch));
    assert_eq!(pushed, RunStart::Branch("fix".into()));
    // A tag the forge reports as the branch is no branch of the repository.
    assert_eq!(RunStart::of(&branch, None), RunStart::Unbranched);
    for unbranched in [run(Some("main"), None), run(None, None)] {
        assert_eq!(other(&unbranched), None);
    }
}

#[test]
fn a_run_is_named_by_its_link_or_its_number() {
    for flagged in [false, true] {
        let named = launch(flagged, |_| {});
        assert_eq!(named_run(&named).unwrap().unwrap().to_string(), RUN);
    }
    let beside_repository = launch(true, |launch| {
        launch.target = Some(StartTarget::Repository("app".into()));
    });
    assert!(named_run(&beside_repository).is_ok());
    let numbered = Launch {
        ci: Some(ItemChoice::Named(ItemInput::Number(2765))),
        ..Default::default()
    };
    assert_eq!(named_run(&numbered).unwrap(), Some(ItemInput::Number(2765)));
    let issue = Launch {
        ci: Some(ItemChoice::Named(
            "https://forge.example/team/app/issues/3".parse().unwrap(),
        )),
        ..Default::default()
    };
    assert!(named_run(&issue).is_err());
}

#[test]
fn a_run_refuses_what_names_the_work_another_way() {
    let picked = Launch {
        ci: Some(ItemChoice::Pick),
        ..Default::default()
    };
    ensure_run_fits(&picked).unwrap();
    let refused: [fn(&mut Launch); 5] = [
        |launch| launch.issue = Some(ItemChoice::Pick),
        |launch| launch.pr = Some(ItemChoice::Pick),
        |launch| launch.branch = Some("fix".into()),
        |launch| launch.copy = Some(SpaceChoice::Current),
        |launch| launch.in_workspace = Some(SpaceChoice::Current),
    ];
    for (index, change) in refused.into_iter().enumerate() {
        for flagged in [false, true] {
            let launch = launch(flagged, change);
            assert!(ensure_run_fits(&launch).is_err(), "{index} {flagged}");
        }
    }
    let twice = launch(true, |launch| {
        launch.target = Some("https://forge.example/team/app/pull/3".parse().unwrap());
    });
    assert!(ensure_run_fits(&twice).is_err());
    let mut picked = picked;
    picked.branch = Some("fix".into());
    assert!(ensure_run_fits(&picked).is_err());
}

#[test]
fn a_settled_run_starts_on_its_pr_its_branch_or_nothing() {
    let settled = |read: Run, start: RunStart, named: Option<&str>| {
        let mut launch = launch(false, |launch| launch.name = named.map(Into::into));
        let url = RUN.parse::<ItemRef>().unwrap();
        settle(&mut launch, (url, &read), start, "repo-id".into()).unwrap();
        assert_eq!(launch.repository.as_deref(), Some("repo-id"));
        let kept = launch.ci.as_ref().and_then(ItemChoice::named);
        assert_eq!(kept.unwrap().to_string(), RUN);
        launch
    };
    let pull = settled(
        run(None, Some(12)),
        RunStart::Pull(PullNumber::new(12)),
        None,
    );
    let Some(StartTarget::Pull(pull_url)) = &pull.target else {
        panic!("not on its PR: {:?}", pull.target);
    };
    assert_eq!(pull_url.url(), "https://forge.example/team/app/pulls/12");
    assert_eq!((pull.branch, pull.name), (None, None));
    let branch = settled(run(Some("fix"), None), RunStart::Branch("fix".into()), None);
    assert_eq!(
        (branch.target, branch.branch.as_deref()),
        (None, Some("fix"))
    );
    assert_eq!(branch.name, None);
    let tag = settled(run(Some("v1.4.0"), None), RunStart::Unbranched, None);
    assert_eq!((tag.target, tag.branch), (None, None));
    assert_eq!(tag.name.unwrap(), "release.yml run 2765 on v1.4.0");
    let named = settled(run(None, None), RunStart::Unbranched, Some("fix it"));
    assert_eq!(named.name.unwrap(), "fix it");
}
