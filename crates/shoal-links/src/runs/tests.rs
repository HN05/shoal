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

#[tokio::test]
async fn a_list_of_every_branchs_runs_names_no_branch() {
    use shoal_core::test_support::{git, global_config, repository};
    use shoal_daemon_base::{
        config_state::ConfigState, file_access::FileAccess, forge_access::ForgeAccess,
        gates::Gates, notifications::Notifications, operations::Operations,
    };
    use shoal_forge::test_support::Server;
    use shoal_protocol::{paths::Paths, protocol::ItemContext, space_names::SpaceSelector};

    use crate::stand_in::{StandIn, StandInRecords};

    let root = tempfile::tempdir().unwrap();
    let store = shoal_store::test_support::unsynced_store(root.path().join("state.db"))
        .await
        .unwrap();
    let server = Server::start(|_| {
        let runs = serde_json::json!({"workflow_runs": []});
        (200, Vec::new(), runs.to_string().into())
    });
    let host = &server.host;
    shoal_forge_types::hosts::learn(host, shoal_forge_types::backends::ForgeKind::Forgejo);
    let text = format!("[machine.forges.\"{host}\"]\ntype = \"forgejo\"\nuser_token = \"u\"\n");
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
    let workspace = shoal_protocol::test_support::workspace("workspace", "repo", "fix", &path);
    let gates = Arc::new(Gates::default());
    let config = Arc::new(ConfigState::new(global_config(&text)));
    let forges = ForgeAccess::new(store.clone(), gates.clone(), config.clone());
    let links = Links::new(
        store.clone(),
        gates.clone(),
        config,
        Arc::new(Notifications::new(store)),
        Arc::new(FileAccess::default()),
        Arc::new(forges),
        Arc::new(Operations::new(Paths::for_test(root.path()), gates)),
        Arc::new(StandInRecords),
    );
    let daemon = StandIn {
        workspaces: vec![workspace.clone()],
        config: Some(global_config(&text)),
        ..StandIn::default()
    };
    let target = ItemContext::space(SpaceSelector::id(&workspace.id)).into();
    let filter = RunFilter {
        branch: None,
        workflow: None,
        commit: None,
        limit: 20,
    };

    let listed = (links.list_ci_runs(&daemon, &target, RunsOf::EveryBranch, filter, None))
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
