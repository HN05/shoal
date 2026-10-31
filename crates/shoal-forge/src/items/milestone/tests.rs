use super::*;
use crate::{
    backends::{Backend, ForgeKind},
    test_support::{Server, agent_on, repo_on},
    transport::memory::Memory,
};
use serde_json::json;

#[test]
fn each_forge_lists_milestones_and_names_their_ids_its_way() {
    let listed = json!({"id": 101, "number": 5, "title": "v2", "name": "v2"});
    let github = ForgeRepo::parse("https://github.com/team/repo").unwrap();
    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    let gitlab = ForgeRepo::parse("git@gitlab.com:group/sub/repo.git").unwrap();
    let bitbucket = ForgeRepo::parse("git@bitbucket.org:workspace/repo.git").unwrap();
    assert_eq!(github.kind.milestone(&listed), Some(("v2".into(), 5)));
    assert_eq!(forgejo.kind.milestone(&listed), Some(("v2".into(), 101)));
    assert_eq!(gitlab.kind.milestone(&listed), Some(("v2".into(), 101)));
    assert_eq!(
        bitbucket.kind.milestone(&json!({"id": 3, "name": "v2"})),
        Some(("v2".into(), 3))
    );
    assert_eq!(forgejo.kind.milestone(&json!({"title": "v2"})), None);
    assert_eq!(
        github.kind.milestones_endpoint(&github),
        "repos/team/repo/milestones?state=all"
    );
    assert_eq!(
        gitlab.kind.milestones_endpoint(&gitlab),
        "projects/group%2Fsub%2Frepo/milestones?include_ancestors=true"
    );
    assert_eq!(
        bitbucket.kind.milestones_endpoint(&bitbucket),
        "repositories/workspace/repo/milestones"
    );
}

#[tokio::test]
async fn a_milestone_is_found_by_its_exact_title() {
    let server = Server::start(|_| {
        let listed = r#"[{"number":4,"title":"V2"},{"number":5,"title":"v2"}]"#;
        (200, Vec::new(), listed.into())
    });
    let account = agent_on(&server, "agent").remembering(Memory::default());
    let github = repo_on(&server, ForgeKind::GitHub, "team/repo");
    assert_eq!(github.milestone_id(&account, "v2").await.unwrap(), 5);
    assert_eq!(
        github
            .milestone_id(&account, "v3")
            .await
            .unwrap_err()
            .to_string(),
        "team/repo has no milestone v3"
    );
    let lines: Vec<_> = server
        .received()
        .into_iter()
        .map(|request| request.line)
        .collect();
    assert!(
        lines[0].starts_with("GET /api/v3/repos/team/repo/milestones?state=all"),
        "{lines:?}"
    );
}
