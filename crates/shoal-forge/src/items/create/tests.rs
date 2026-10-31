use super::*;
use crate::{
    backends::ForgeKind,
    test_support::{Server, agent_on, repo_on},
    transport::{api::HttpMethod, memory::Memory},
};
use serde_json::json;

#[tokio::test]
async fn an_issue_whose_read_fails_after_posting_comes_back_as_posted() {
    // Posting spends the account's last request until the limit resets.
    let server = Server::start(|_| {
        let limit = ["X-RateLimit-Remaining: 0", "X-RateLimit-Reset: 600"];
        (
            201,
            limit.map(String::from).into(),
            r#"{"number":7}"#.into(),
        )
    });
    let account = agent_on(&server, "agent").remembering(Memory::default());
    let github = repo_on(&server, ForgeKind::GitHub, "team/repo");
    let issue = NewIssue {
        title: "Flaky".into(),
        body: "Seen twice".into(),
        labels: vec!["bug".into()],
        assignees: Vec::new(),
        milestone: None,
        attachments: Vec::new(),
    };
    let created = github.create_issue(&account, &issue).await.unwrap();
    assert_eq!(
        created.item,
        Item {
            kind: ItemKind::Issue,
            number: 7,
            url: format!("http://{}/team/repo/issues/7", server.host),
            title: "Flaky".into(),
            state: ItemState::Open,
            labels: vec!["bug".into()],
            assignees: Vec::new(),
            milestone: None,
            pr: None,
        }
    );
    let unfinished = created.unfinished.unwrap();
    assert!(
        unfinished.starts_with("issue #7 was opened, but reading it failed"),
        "{unfinished}"
    );
    // The read was refused without reaching the forge.
    assert_eq!(server.received().len(), 1);
}

#[test]
fn issues_carry_labels_on_github_and_label_afterwards_on_forgejo() {
    let issue = NewIssue {
        title: "Flaky".into(),
        body: "Seen twice".into(),
        labels: vec!["bug".into()],
        assignees: Vec::new(),
        milestone: None,
        attachments: Vec::new(),
    };
    let github = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
    assert_eq!(
        github.issue_request(&issue),
        Request::new(
            HttpMethod::Post,
            "repos/team/repo/issues".into(),
            Some(json!({"title": "Flaky", "body": "Seen twice", "labels": ["bug"]})),
        )
    );
    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    assert_eq!(
        forgejo.issue_request(&issue).body,
        Some(json!({"title": "Flaky", "body": "Seen twice"}))
    );
    assert!(github.created_number(&json!({"id": 1})).is_err());
}

#[test]
fn gitlab_opens_merge_requests_with_draft_titles_and_labeled_issues() {
    let gitlab = ForgeRepo::parse("git@gitlab.com:group/sub/repo.git").unwrap();
    let pull = NewPull {
        title: "Fix".into(),
        body: "Closes #3".into(),
        head: "topic".into(),
        base: "main".into(),
        draft: true,
        ..NewPull::default()
    };
    assert_eq!(
        gitlab.create_pull_request(&pull),
        Request::new(
            HttpMethod::Post,
            "projects/group%2Fsub%2Frepo/merge_requests".into(),
            Some(json!({"title": "Draft: Fix", "description": "Closes #3",
                    "source_branch": "topic", "target_branch": "main"})),
        )
    );
    let issue = NewIssue {
        title: "Flaky".into(),
        body: "Seen twice".into(),
        labels: vec!["bug".into(), "ci".into()],
        assignees: Vec::new(),
        milestone: None,
        attachments: Vec::new(),
    };
    assert_eq!(
        gitlab.issue_request(&issue).body,
        Some(json!({"title": "Flaky", "description": "Seen twice", "labels": "bug,ci"}))
    );
    assert_eq!(
        gitlab.created_number(&json!({"iid": 4, "id": 90})).unwrap(),
        4
    );
    let head = Head {
        repository: "group/sub/repo",
        branch: "topic",
    };
    let listed = json!({"source_branch": "topic", "source_project_id": 1, "target_project_id": 1});
    assert!(gitlab.kind.has_head(&gitlab, &listed, &head));
    let fork = json!({"source_branch": "topic", "source_project_id": 2, "target_project_id": 1});
    assert!(!gitlab.kind.has_head(&gitlab, &fork, &head));
    let forked = Head {
        repository: "someone/repo",
        ..head
    };
    assert!(!gitlab.kind.has_head(&gitlab, &listed, &forked));
    assert!(gitlab.require(Capability::ForkPrs).is_err());
}

#[test]
fn bitbucket_opens_pull_requests_and_issues_without_labels() {
    let bitbucket = ForgeRepo::parse("git@bitbucket.org:workspace/repo.git").unwrap();
    let pull = NewPull {
        title: "Fix".into(),
        body: "Closes #3".into(),
        head: "topic".into(),
        base: "main".into(),
        draft: true,
        ..NewPull::default()
    };
    assert_eq!(
        bitbucket.create_pull_request(&pull),
        Request::new(
            HttpMethod::Post,
            "repositories/workspace/repo/pullrequests".into(),
            Some(
                json!({"title": "Fix", "description": "Closes #3", "draft": true,
                    "source": {"branch": {"name": "topic"}},
                    "destination": {"branch": {"name": "main"}}})
            ),
        )
    );
    let issue = NewIssue {
        title: "Flaky".into(),
        body: "Seen twice".into(),
        labels: Vec::new(),
        assignees: Vec::new(),
        milestone: None,
        attachments: Vec::new(),
    };
    assert_eq!(
        bitbucket.issue_request(&issue).body,
        Some(json!({"title": "Flaky", "content": {"raw": "Seen twice"}}))
    );
    assert!(bitbucket.require_all(issue.needs()).is_ok());
    let labeled = NewIssue {
        labels: vec!["bug".into()],
        ..issue
    };
    assert!(bitbucket.require_all(labeled.needs()).is_err());
    assert_eq!(bitbucket.created_number(&json!({"id": 9})).unwrap(), 9);
    let listed = json!({"source": {"branch": {"name": "topic"},
            "repository": {"full_name": "Workspace/Repo"}}});
    let head = |repository, branch| Head { repository, branch };
    assert!(
        bitbucket
            .kind
            .has_head(&bitbucket, &listed, &head("workspace/repo", "topic"))
    );
    assert!(
        !bitbucket
            .kind
            .has_head(&bitbucket, &listed, &head("workspace/repo", "other"))
    );
    assert!(
        !bitbucket
            .kind
            .has_head(&bitbucket, &listed, &head("someone/repo", "topic"))
    );
    let forked = NewPull {
        head_repository: Some("someone/repo".into()),
        ..pull
    };
    assert_eq!(
        bitbucket.create_pull_request(&forked).body.unwrap()["source"],
        json!({"branch": {"name": "topic"}, "repository": {"full_name": "someone/repo"}})
    );
}

#[test]
fn drafts_are_a_flag_on_github_and_a_title_on_forgejo() {
    let pull = NewPull {
        title: "Fix".into(),
        body: "Closes #3".into(),
        head: "topic".into(),
        base: "main".into(),
        draft: true,
        ..NewPull::default()
    };
    let github = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
    assert_eq!(
        github.create_pull_request(&pull),
        Request::new(
            HttpMethod::Post,
            "repos/team/repo/pulls".into(),
            Some(json!({"title": "Fix", "body": "Closes #3", "head": "topic",
                    "base": "main", "draft": true})),
        )
    );
    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    assert_eq!(
        forgejo.create_pull_request(&pull).body,
        Some(json!({"title": "WIP: Fix", "body": "Closes #3", "head": "topic", "base": "main"}))
    );
}

#[test]
fn github_and_forgejo_name_a_fork_head_by_its_owner() {
    let pull = NewPull {
        title: "Fix".into(),
        head: "topic".into(),
        head_repository: Some("someone/repo".into()),
        base: "main".into(),
        ..NewPull::default()
    };
    for remote in [
        "git@github.com:team/repo.git",
        "https://forge.example/team/repo",
    ] {
        let forge = ForgeRepo::parse(remote).unwrap();
        assert_eq!(
            forge.create_pull_request(&pull).body.unwrap()["head"],
            "someone:topic"
        );
        assert!(forge.require(Capability::ForkPrs).is_ok());
        let listed = json!({"head": {"ref": "topic", "repo": {"full_name": "Someone/Repo"}}});
        let head = |repository| Head {
            repository,
            branch: "topic",
        };
        assert!(forge.kind.has_head(&forge, &listed, &head("someone/repo")));
        assert!(!forge.kind.has_head(&forge, &listed, &head("team/repo")));
    }
}

#[test]
fn each_forge_reads_a_branch_commit_from_its_own_route() {
    for (remote, endpoint, pointer) in [
        (
            "git@github.com:team/repo.git",
            "repos/team/repo/branches/fix/a",
            "/commit/sha",
        ),
        (
            "https://forge.example/team/repo",
            "repos/team/repo/branches/fix%2Fa",
            "/commit/id",
        ),
        (
            "git@gitlab.com:team/repo.git",
            "projects/team%2Frepo/repository/branches/fix%2Fa",
            "/commit/id",
        ),
        (
            "git@bitbucket.org:team/repo.git",
            "repositories/team/repo/refs/branches/fix%2Fa",
            "/target/hash",
        ),
    ] {
        let forge = ForgeRepo::parse(remote).unwrap();
        let (request, read) = forge.kind.branch_read(&forge, "fix/a");
        assert_eq!((request.endpoint.as_str(), read), (endpoint, pointer));
    }
    let fork = ForgeRepo::parse("https://github.com/team/repo")
        .unwrap()
        .sibling("someone/repo");
    assert_eq!(
        fork,
        ForgeRepo::parse("https://github.com/someone/repo").unwrap()
    );
}

#[test]
fn new_items_need_the_assignee_and_milestone_capabilities_of_their_kind() {
    let issue = NewIssue {
        assignees: vec!["kim".into()],
        milestone: Some("v2".into()),
        ..NewIssue::default()
    };
    assert_eq!(
        issue.needs(),
        [Capability::IssueAssignees, Capability::IssueMilestones]
    );
    let pull = NewPull {
        assignees: vec!["kim".into()],
        ..NewPull::default()
    };
    assert_eq!(pull.needs(), [Capability::PrAssignees]);
    let bitbucket = ForgeRepo::parse("git@bitbucket.org:workspace/repo.git").unwrap();
    assert!(bitbucket.require_all(issue.needs()).is_ok());
    assert!(bitbucket.require_all(pull.needs()).is_err());
}
