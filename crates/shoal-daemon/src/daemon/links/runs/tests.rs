use super::*;

fn started(url: &str) -> StartedRun {
    StartedRun {
        workflow: Some("deploy".into()),
        branch: "topic".into(),
        url: Some(url.into()),
        linked: false,
        warnings: Vec::new(),
    }
}

#[test]
fn a_started_run_is_linked_only_where_shoal_reads_runs() -> Result<()> {
    let forgejo = ForgeRepo::parse("https://codeberg.org/team/repo")?;
    let link = started_link(
        &forgejo,
        &started("https://codeberg.org/team/repo/actions/runs/2765"),
    );
    assert_eq!(
        link.map(|item| item.url).as_ref().map(|url| url.url()),
        Some("https://codeberg.org/team/repo/actions/runs/2765")
    );
    // Bitbucket reports a pipeline's URL but Shoal reads no pipelines, so
    // the started pipeline goes unlinked rather than failing the start.
    let bitbucket = ForgeRepo::parse("https://bitbucket.org/team/repo")?;
    let pipeline = started("https://bitbucket.org/team/repo/pipelines/results/7");
    assert_eq!(started_link(&bitbucket, &pipeline), None);
    Ok(())
}

#[test]
fn a_chat_starts_runs_only_through_a_workspace_below_it() {
    // A chat has no branch, so it names a workspace below it instead.
    assert!(on_own_branch_name(None, "topic").is_err());
}

#[tokio::test]
async fn only_a_chat_without_a_repository_watches_runs_beyond_its_own_repository() {
    use crate::{
        client::space_names::SpaceSelector,
        daemon::test_support::{chat, git, manager, repository},
        forge::items::link::Selection,
    };
    let (root, manager) = manager().await;
    let manager = std::sync::Arc::new(manager);
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "runs".into(), None, None, None)
        .await
        .unwrap();
    git(
        &path,
        &["remote", "add", "origin", "https://codeberg.org/team/repo"],
    );
    let master = chat(&manager, "master", "orchestrate", None, None).await;
    let run = Selection {
        kind: Some(LinkKind::Ci),
        input: Some(
            "https://codeberg.org/team/other/actions/runs/7"
                .parse()
                .unwrap(),
        ),
    };
    let watch = |space: &SpaceId| {
        let space = SpaceSelector::id(space);
        let (manager, run) = (manager.clone(), run.clone());
        async move { manager.ensure_own_repository_run(&space, &run).await }
    };
    let refused = watch(&workspace.id).await.unwrap_err().to_string();
    assert_eq!(refused, place::ELSEWHERE);
    // The chat's watch reads registered repositories alone, each its own.
    watch(&master.id).await.unwrap();
}
