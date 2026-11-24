use crate::stand_in::Fixture;
use shoal_protocol::launches::{ItemChoice, Launch, LaunchSource, SpaceChoice, StartTarget};
use shoal_protocol::space_names::SpaceSelector;

#[test]
fn a_launch_holds_each_commit_it_carried_once() {
    let commit = shoal_refs::CommitId::from("0123456789abcdef0123456789abcdef01234567");
    let files = shoal_refs::CommitId::from("89abcdef0123456789abcdef0123456789abcdef");
    let launch = |files: &shoal_refs::CommitId| shoal_protocol::launches::Launch {
        forwarded: Some(shoal_protocol::launches::Carried {
            origin: Some("https://forge.example/team/app".into()),
            commit: Some(commit.clone()),
            ..Default::default()
        }),
        carried_copy: Some(shoal_protocol::launches::CarriedCopy {
            files: files.clone(),
            name: "fix-copy".into(),
        }),
        ..Default::default()
    };
    let held = |files| crate::launches::carried_commits(&launch(files)).unwrap().1;
    assert_eq!(held(&files), [commit.clone(), files.clone()]);
    // A copy without unsaved files carried them as its base.
    assert_eq!(held(&commit), std::slice::from_ref(&commit));
}

/// A repository whose origin no fetch reaches, on a machine that starts
/// `claude` and has a chat mode without a repository.
async fn offline_machine(manager: &Fixture) -> crate::model::Repository {
    crate::stand_in::configure(
        manager,
        "[machine]\nmax_heavy_agents = 1000\n[machine.overload.disk]\nenabled = false\n\
         [modes.solo]\nkind = 'chat'\nrepository = false\n\
         [agents.claude]\ncommand = ['sh']\n",
    )
    .await;
    let repo = manager.repository("repo").await;
    crate::stand_in::git(&repo.path, &["config", "protocol.allow", "never"]);
    let remote = ["remote", "add", "origin", "https://github.com/team/repo"];
    crate::stand_in::git(&repo.path, &remote);
    repo
}

/// The launch of a workspace in `repo` on `branch`, as a start's pickers
/// leave it.
fn launch_on_branch(repo: &crate::model::Repository, branch: Option<&str>) -> Launch {
    Launch {
        target: Some(StartTarget::Repository(repo.id.to_string())),
        branch: branch.map(Into::into),
        ..Launch::default()
    }
}

/// A check of a chat in a workspace's worktree answers with that
/// workspace, and refuses it as the chat's parent, as the start does.
#[tokio::test]
async fn a_chat_check_answers_its_host_and_refuses_it_as_parent() {
    let manager = Fixture::new().await;
    let repo = offline_machine(&manager).await;
    let base = Some("main".into());
    let fix = (manager.create_workspace(&repo.id, "fix".into(), base, None, None))
        .await
        .unwrap();
    let hosted = Launch {
        in_workspace: Some(SpaceChoice::Space(SpaceSelector::id(&fix.id))),
        mode: Some("chat".into()),
        ..Launch::default()
    };
    let below_host = Launch {
        parent: Some(SpaceSelector::id(&fix.id)),
        ..hosted.clone()
    };

    let checked = manager
        .spaces
        .check_launch(&*manager, hosted, None)
        .await
        .unwrap();

    assert_eq!(checked.map(|workspace| workspace.id), Some(fix.id));
    let refused = manager
        .spaces
        .check_launch(&*manager, below_host, None)
        .await
        .unwrap_err();
    assert!(
        format!("{refused:#}").contains("cannot go below it"),
        "{refused:#}"
    );
}

/// A check of a plan that asks for its issue leaves it to a picker, which
/// a start without one refuses.
#[tokio::test]
async fn a_chat_check_leaves_a_plan_issue_to_a_picker() {
    let manager = Fixture::new().await;
    let repo = offline_machine(&manager).await;
    let launch = Launch {
        target: Some(StartTarget::Repository(repo.id.to_string())),
        mode: Some("plan".into()),
        issue: Some(ItemChoice::Pick),
        agent: Some("claude".into()),
        ..Launch::default()
    };

    let checked = manager
        .spaces
        .check_launch(&*manager, launch.clone(), None)
        .await;

    assert!(checked.unwrap().is_none());
    let started = manager
        .spaces
        .launch(&manager.daemon(), launch, LaunchSource::Manual, None)
        .await;
    assert!(format!("{:#}", started.unwrap_err()).contains("missing issue"));
}

/// A check of a launch on the branch a workspace has answers with that
/// workspace, which the start goes into, and refuses what only a new
/// workspace takes, as the start does.
#[tokio::test]
async fn a_check_answers_the_workspace_that_has_the_branch() {
    let manager = Fixture::new().await;
    let repo = offline_machine(&manager).await;
    let base = Some("main".into());
    let fix = (manager.create_workspace(&repo.id, "fix".into(), base, None, None))
        .await
        .unwrap();
    let named = Launch {
        name: Some("other".into()),
        ..launch_on_branch(&repo, Some("fix"))
    };

    let checked =
        manager
            .spaces
            .check_launch(&*manager, launch_on_branch(&repo, Some("fix")), None);

    assert_eq!(
        checked.await.unwrap().map(|workspace| workspace.id),
        Some(fix.id)
    );
    assert!(
        manager
            .spaces
            .check_launch(&*manager, named, None)
            .await
            .is_err()
    );
}

/// A check refuses what creating its workspace would refuse, and creates
/// nothing.
#[tokio::test]
async fn a_check_refuses_what_creation_would_and_creates_nothing() {
    let manager = Fixture::new().await;
    let repo = offline_machine(&manager).await;
    let launch = Launch {
        git_profile: Some("missing".into()),
        ..launch_on_branch(&repo, Some("fix"))
    };

    assert!(
        manager
            .spaces
            .check_launch(&*manager, launch, None)
            .await
            .is_err()
    );
    assert!(manager.spaces.list_workspaces().await.unwrap().is_empty());
}

/// A check leaves open what a picker has yet to choose: a launch that
/// names only a base, which its start would begin as a chat and refuse,
/// passes until a branch is picked.
#[tokio::test]
async fn a_check_leaves_the_branch_to_a_picker() {
    let manager = Fixture::new().await;
    let repo = offline_machine(&manager).await;
    let launch = Launch {
        base: Some("main".into()),
        agent: Some("claude".into()),
        ..launch_on_branch(&repo, None)
    };

    let checked = manager
        .spaces
        .check_launch(&*manager, launch.clone(), None)
        .await;

    assert!(checked.unwrap().is_none());
    let started = manager
        .spaces
        .launch(&manager.daemon(), launch, LaunchSource::Manual, None)
        .await;
    assert!(format!("{:#}", started.unwrap_err()).contains("--base"));
}
