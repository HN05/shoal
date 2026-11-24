use shoal_forge_types::ci::run_view::RunFilter;
use shoal_ids::space_ids::SpaceId;
use shoal_links::runs::place::ELSEWHERE;
use shoal_protocol::protocol::{ItemContext, RunsOf};
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    daemon::test_support::{chat, git, manager, repository},
    forge::items::link::{LinkKind, Selection},
};

#[tokio::test]
async fn only_a_chat_without_a_repository_watches_runs_beyond_its_own_repository() {
    let (root, manager) = manager().await;
    // Settled in config, the forge's type needs no probe of the network.
    let forge = "[machine.forges.\"codeberg.org\"]\ntype = \"forgejo\"\n";
    crate::daemon::test_support::configure(&manager, forge).await;
    let manager = std::sync::Arc::new(manager);
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
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
        async move {
            (manager.links)
                .ensure_own_repository_run(&**manager, &space, &run)
                .await
        }
    };
    let refused = watch(&workspace.id).await.unwrap_err().to_string();
    assert_eq!(refused, ELSEWHERE);
    // The chat's watch reads registered repositories alone, each its own.
    watch(&master.id).await.unwrap();
}

#[tokio::test]
async fn a_list_of_every_branchs_runs_names_no_branch() {
    let (root, manager) = manager().await;
    let server = shoal_forge::test_support::Server::start(|_| {
        let runs = serde_json::json!({"workflow_runs": []});
        (200, Vec::new(), runs.to_string().into())
    });
    let host = server.host.clone();
    crate::forge::hosts::learn(&host, crate::forge::backends::ForgeKind::Forgejo);
    let forge = format!("[machine.forges.\"{host}\"]\ntype = \"forgejo\"\nuser_token = \"u\"\n");
    crate::daemon::test_support::configure(&manager, &forge).await;
    let path = repository(root.path(), "repo");
    git(
        &path,
        &[
            "remote",
            "add",
            "origin",
            &format!("http://{host}/team/repo"),
        ],
    );
    let path = path.to_str().unwrap().into();
    let repo = manager.repositories.register(path, None, None).await;
    let target = ItemContext::Checkout(repo.unwrap().id).into();
    let filter = RunFilter {
        branch: None,
        workflow: None,
        commit: None,
        limit: 20,
    };

    let listed = (manager.links)
        .list_ci_runs(&*manager, &target, RunsOf::EveryBranch, filter, None)
        .await
        .unwrap();

    assert!(listed.is_empty());
    let lines: Vec<_> = server.received().into_iter().map(|r| r.line).collect();
    assert!(
        lines.iter().any(|line| line.contains("actions/runs")),
        "{lines:?}"
    );
    assert!(lines.iter().all(|line| !line.contains("ref=")), "{lines:?}");
}
