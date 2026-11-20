use super::*;
use crate::daemon::test_support::{configure, git, manager, repository};
use crate::model::SpaceKind;
use serde_json::json;
use shoal_forge::test_support::Server;
use shoal_protocol::config::modes::ModeName;
use shoal_protocol::launches::SeveralModes;

/// The modes whose labels call for them, two of them in a workspace.
const LABELLED: &str = "[modes.plan]\nlabel = \"needs-plan\"\n\
     [modes.worker]\nlabel = \"planned\"\n\
     [modes.hotfix]\nkind = \"workspace\"\nlabel = \"hotfix\"\n";

/// A manager under [`LABELLED`] with a repository whose origin is a
/// Forgejo `team/repo` whose open issue 7 carries plan's label, 8 those
/// of plan and the worker, 9 no mode's and 10 those of both workspace
/// modes, with the repository's ID.
async fn labelled_forge() -> (tempfile::TempDir, std::sync::Arc<Manager>, Server, String) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repository = (manager.register_repository(path.to_str().unwrap().into(), None, None))
        .await
        .unwrap();
    let server = Server::start(|request| {
        let (number, labels) = match request.line.as_str() {
            "GET /api/v1/repos/team/repo/issues/7" => (7, vec!["needs-plan", "type/bug"]),
            "GET /api/v1/repos/team/repo/issues/8" => (8, vec!["needs-plan", "planned"]),
            "GET /api/v1/repos/team/repo/issues/9" => (9, vec!["type/bug"]),
            "GET /api/v1/repos/team/repo/issues/10" => (10, vec!["planned", "hotfix"]),
            _ => return (404, Vec::new(), Vec::new()),
        };
        let labels: Vec<_> = (labels.into_iter())
            .map(|name| json!({"name": name}))
            .collect();
        let issue = json!({"number": number, "title": "Bug", "state": "open", "labels": labels});
        (200, Vec::new(), issue.to_string().into())
    });
    let host = &server.host;
    crate::forge::hosts::learn(host, crate::forge::backends::ForgeKind::Forgejo);
    let forge = format!("[machine.forges.\"{host}\"]\ntype = \"forgejo\"\nuser_token = \"user\"\n");
    configure(&manager, &format!("{LABELLED}{forge}")).await;
    let origin = format!("http://{host}/team/repo");
    git(&path, &["remote", "add", "origin", &origin]);
    (root, manager, server, repository.id.to_string())
}

/// The launch of an agent on `issue` of `repository`, naming no mode.
fn on_issue(repository: &str, issue: &str) -> Launch {
    Launch {
        target: Some(issue.parse().unwrap()),
        repository: Some(repository.to_owned()),
        ..Launch::default()
    }
}

/// A launch that names no mode takes the one its issue's labels call for,
/// and the kind of space that mode starts, as every caller's launch does;
/// one whose issue carries no mode's label takes the default mode.
#[tokio::test]
async fn a_launch_without_a_mode_takes_the_one_its_issues_labels_call_for() {
    let (_root, manager, _server, repository) = labelled_forge().await;
    let shape = |issue| {
        let launch = on_issue(&repository, issue);
        let manager = manager.clone();
        async move { manager.launch_shape(&launch, None).await.unwrap() }
    };
    let labelled = shape("7").await;
    assert_eq!(
        (labelled.mode, labelled.kind),
        (ModeName::PLAN, SpaceKind::Chat)
    );
    let shape = shape("9").await;
    assert_eq!(
        (shape.mode, shape.kind),
        (ModeName::DEFAULT, SpaceKind::Workspace)
    );
}

/// An issue carrying the labels of several modes refuses a launch that
/// names none, naming them, since no default is right; a client reads the
/// same answer to offer its picker.
#[tokio::test]
async fn a_launch_on_an_issue_with_the_labels_of_several_modes_is_refused() {
    let (_root, manager, _server, repository) = labelled_forge().await;
    let launch = on_issue(&repository, "8");
    let error = manager.launch_shape(&launch, None).await.err().unwrap();
    let several = error.downcast_ref::<SeveralModes>().unwrap();
    assert_eq!(several.item, "issue 8");
    let modes: Vec<_> = (several.modes.iter()).map(|(mode, _)| mode).collect();
    assert_eq!(modes, [&ModeName::PLAN, &ModeName::DEFAULT]);
    let answered = manager.launch_labelled_mode(launch, None).await.unwrap();
    assert_eq!(answered, LabelledMode::Several(several.clone()));
}

/// A launch that names its mode takes it whatever its issue's labels say,
/// without reading the issue.
#[tokio::test]
async fn a_named_mode_reads_no_labels() {
    let (_root, manager, server, repository) = labelled_forge().await;
    let launch = Launch {
        mode: Some(ModeName::DEFAULT),
        ..on_issue(&repository, "8")
    };
    let shape = manager.launch_shape(&launch, None).await.unwrap();
    assert_eq!(shape.mode, ModeName::DEFAULT);
    let read = (server.received().iter()).any(|request| request.line.contains("/issues/"));
    assert!(!read, "the issue was read");
}

/// A check of a launch whose item carries the labels of several modes
/// leaves the mode to a terminal's picker instead of refusing it, even
/// where its other arguments decide the kind of space.
#[tokio::test]
async fn a_check_leaves_several_labelled_modes_to_a_picker() {
    let (_root, manager, _server, repository) = labelled_forge().await;
    let launch = Launch {
        branch: Some("fix".into()),
        ..on_issue(&repository, "10")
    };
    assert!(launches::decides_kind(&launch));
    let labelled = manager.labelled_launch_mode(&launch, None).await.unwrap();
    assert!(matches!(labelled, LabelledMode::Several(_)), "{labelled:?}");
    manager.check_launch(launch, None).await.unwrap();
}
