use super::*;
use crate::transport::api::TransportRequests;
use shoal_forge_types::backends::ForgeKind;
use shoal_forge_types::items::link::LinkKind;
use shoal_forge_types::items::{
    item::{Current, Item, ItemState, PrSummary},
    link::ItemKind,
};

fn pull(head_repository: Option<&str>) -> Current {
    Current {
        item: Item {
            kind: ItemKind::Pr,
            number: 7,
            url: "https://forge.example/team/repo/pulls/1".parse().unwrap(),
            title: "Fix".into(),
            state: ItemState::Open,
            labels: Vec::new(),
            assignees: Vec::new(),
            milestone: None,
            pr: Some(PrSummary {
                head: "topic".into(),
                base: "main".into(),
                draft: false,
            }),
        },
        node_id: None,
        head_repository: head_repository.map(str::to_owned),
        body: String::new(),
        author: None,
        head_sha: Some("a".repeat(40).into()),
        base_sha: None,
        merge_commit: None,
        conflict: None,
        merges_now: false,
        reviewers: Vec::new(),
        user_ids: Default::default(),
        milestone_id: None,
        replaced_labels: Vec::new(),
    }
}

#[test]
fn a_prs_head_is_in_a_fork_unless_this_repository_has_it() {
    let forge = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    let details = |repository| {
        forge
            .checkout_details(
                "https://forge.example/team/repo/pulls/7".parse().unwrap(),
                pull(repository),
            )
            .unwrap()
    };
    let own = details(Some("Team/Repo"));
    assert_eq!(
        (own.head.as_str(), own.base.as_str(), own.fork),
        ("topic", "main", false)
    );
    assert_eq!(own.head_commit, Some("a".repeat(40).into()));
    // A deleted fork leaves no head repository.
    for fork in [Some("someone/repo"), None] {
        assert!(details(fork).fork);
    }
    // A read that needs only the base works without a head commit.
    let without_head = Current {
        head_sha: None,
        ..pull(Some("team/repo"))
    };
    let read = forge
        .checkout_details(
            "https://forge.example/team/repo/pulls/7".parse().unwrap(),
            without_head,
        )
        .unwrap();
    assert_eq!((read.base.as_str(), read.head_commit), ("main", None));
}

#[test]
fn bitbucket_items_use_pull_request_routes() {
    let repo = ForgeRepo::parse("git@bitbucket.org:workspace/repo.git").unwrap();
    let base = "https://bitbucket.org/workspace/repo";
    assert_eq!(
        repo.issue("4").unwrap(),
        (IssueNumber::new(4), format!("{base}/issues/4"))
    );
    let url = format!("{base}/pull-requests/5");
    assert_eq!(repo.pull("5").unwrap(), (PullNumber::new(5), url.clone()));
    assert_eq!(
        ForgeRepo::from_item_url(LinkKind::Pr, &format!("{url}/diff")).unwrap(),
        repo
    );
    assert!(repo.pull(&format!("{base}/pulls/5")).is_err());
    assert_eq!(
        repo.repo_endpoint("pullrequests/5"),
        "repositories/workspace/repo/pullrequests/5"
    );
}

#[test]
fn only_gitlab_paths_nest_groups() {
    let repo = ForgeRepo::parse("https://gitlab.com/group/sub/project.git").unwrap();
    assert_eq!(repo.path, "group/sub/project");
    assert_eq!(
        repo.repo_endpoint("issues/3"),
        "projects/group%2Fsub%2Fproject/issues/3"
    );
    for remote in [
        "https://github.com/group/sub/repo",
        "https://forge.example/group/sub/repo",
        "https://gitlab.com/project",
        "https://gitlab.com/group/-/project",
        "https://gitlab.com/group//project",
    ] {
        assert!(ForgeRepo::parse(remote).is_err(), "{remote}");
    }
}

/// Every host serves Forgejo.
struct Forgejo;

impl HostTypes for Forgejo {
    async fn forge_type(&self, _host: &str, _scheme: &'static str) -> Result<HostType> {
        Ok(HostType::Settled(ForgeKind::Forgejo))
    }
}

#[tokio::test]
async fn a_named_repository_is_a_url_or_a_path_on_the_same_host() {
    let repo = ForgeRepo::parse("git@forge.example:team/repo.git").unwrap();
    for (input, expected) in [
        ("team/other", "https://forge.example/team/other"),
        ("someone/tool.git", "https://forge.example/someone/tool"),
        (
            "https://elsewhere.example/them/tool",
            "https://elsewhere.example/them/tool",
        ),
        (
            "git@elsewhere.example:them/tool.git",
            "https://elsewhere.example/them/tool",
        ),
    ] {
        ForgeRepo::check_named(input).unwrap();
        let named = repo.resolve_named(input, &Forgejo).await.unwrap();
        assert_eq!(named.repository_url(), expected, "{input}");
    }
    for input in ["tool", "team/../tool", "team/tool/extra", ""] {
        assert!(
            repo.resolve_named(input, &Forgejo).await.is_err(),
            "{input}"
        );
    }
    // Before any host answers, only what names no repository on any forge
    // is refused; nested groups wait for the forge.
    ForgeRepo::check_named("group/team/tool").unwrap();
    for input in [
        "tool",
        "team/../tool",
        "",
        "https://forge.example/tool",
        "ftp://x/a/b",
    ] {
        assert!(ForgeRepo::check_named(input).is_err(), "{input}");
    }
}
