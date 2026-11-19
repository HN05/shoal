use serde_json::{Value, json};

use super::*;
use crate::test_support::{Received, Server, agent_on, repo_on};
use shoal_forge_types::{backends::ForgeKind, numbers::PullNumber};

/// A forge that answers each request whose line contains a key of
/// `replies` with its JSON, and anything else with 404.
fn forge(replies: Vec<(&'static str, Value)>) -> Server {
    Server::start(move |request: &Received| {
        let reply = replies.iter().find(|(key, _)| request.line.contains(key));
        match reply {
            Some((_, body)) => (200, Vec::new(), body.to_string().into()),
            None => (404, Vec::new(), b"{}".to_vec()),
        }
    })
}

fn branch(name: &str, head: &str, at: &str, protected: bool) -> ForgeBranch {
    ForgeBranch {
        name: name.into(),
        head: head.into(),
        committed_at: Some(at.into()),
        protected,
    }
}

#[tokio::test]
async fn each_forge_lists_branches_with_their_commits_and_protection() {
    let listed = [
        (
            ForgeKind::Forgejo,
            vec![(
                "/branches",
                json!([
                    {"name": "main", "commit": {"id": "a1", "timestamp": "t1"}, "protected": true},
                    {"name": "topic", "commit": {"id": "b2", "timestamp": "t2"}, "protected": false},
                ]),
            )],
        ),
        (
            ForgeKind::GitLab,
            vec![(
                "/repository/branches",
                json!([
                    {"name": "main", "commit": {"id": "a1", "committed_date": "t1"}, "protected": true},
                    {"name": "topic", "commit": {"id": "b2", "committed_date": "t2"}, "protected": false},
                ]),
            )],
        ),
        (
            ForgeKind::GitHub,
            vec![(
                "graphql",
                json!({"data": {"repository": {"refs": {
                "pageInfo": {"hasNextPage": false, "endCursor": null},
                "nodes": [
                    {"name": "main", "target": {"oid": "a1", "committedDate": "t1"},
                        "branchProtectionRule": {"id": "R1"}},
                    {"name": "topic", "target": {"oid": "b2", "committedDate": "t2"},
                        "branchProtectionRule": null},
                ]}}}}),
            )],
        ),
    ];
    for (kind, replies) in listed {
        let server = forge(replies);
        let repo = repo_on(&server, kind, "team/project");
        let branches = repo.branches(&agent_on(&server, "agent")).await.unwrap();
        assert_eq!(
            branches,
            [
                branch("main", "a1", "t1", true),
                branch("topic", "b2", "t2", false)
            ],
            "{kind:?}"
        );
    }
}

#[tokio::test]
async fn each_forge_lists_open_prs_by_the_branches_they_join() {
    let github_shape = json!([
        {"number": 3, "head": {"ref": "topic", "repo": {"full_name": "Team/Project"}},
            "base": {"ref": "main"}},
        {"number": 4, "head": {"ref": "topic", "repo": {"full_name": "someone/project"}},
            "base": {"ref": "release"}},
    ]);
    let listed = [
        (
            ForgeKind::Forgejo,
            "/pulls?state=open",
            github_shape.clone(),
        ),
        (ForgeKind::GitHub, "/pulls?state=open", github_shape),
        (
            ForgeKind::GitLab,
            "/merge_requests?state=opened",
            json!([
                {"iid": 3, "source_branch": "topic", "target_branch": "main",
                    "source_project_id": 1, "target_project_id": 1},
                {"iid": 4, "source_branch": "topic", "target_branch": "release",
                    "source_project_id": 2, "target_project_id": 1},
            ]),
        ),
    ];
    for (kind, path, body) in listed {
        let server = forge(vec![(path, body)]);
        let repo = repo_on(&server, kind, "team/project");
        let pulls = repo.open_pulls(&agent_on(&server, "agent")).await.unwrap();
        let joined = |branch: &str| -> Vec<u64> {
            (pulls.iter())
                .filter(|pull| pull.joins(&branch.into()))
                .map(|pull| pull.number.get())
                .collect()
        };
        assert_eq!(pulls[0].number, PullNumber::new(3), "{kind:?}");
        // A fork's branch of the same name is not this repository's.
        assert_eq!(
            (joined("topic"), joined("main"), joined("release")),
            (vec![3], vec![3], vec![4]),
            "{kind:?}"
        );
    }
}

#[tokio::test]
async fn a_branch_is_deleted_only_at_the_commit_it_was_read_at() {
    let server = forge(vec![
        (
            "GET /api/v1/repos/team/project/branches/topic",
            json!({"commit": {"id": "b2"}}),
        ),
        (
            "DELETE /api/v1/repos/team/project/branches/topic",
            json!({}),
        ),
    ]);
    let repo = repo_on(&server, ForgeKind::Forgejo, "team/project");
    let account = agent_on(&server, "agent");
    let topic = BranchName::from("topic");
    let (read, moved) = (CommitId::from("b2"), CommitId::from("a1"));
    let refused = repo.delete_branch(&account, &topic, &moved).await;
    assert!(refused.is_err());
    let deletes = |server: &Server| {
        (server.received().iter())
            .filter(|request| request.line.starts_with("DELETE"))
            .count()
    };
    assert_eq!(deletes(&server), 0);
    repo.delete_branch(&account, &topic, &read).await.unwrap();
    assert_eq!(deletes(&server), 1);
}
