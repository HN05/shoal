use super::*;

#[test]
fn links_of_unresolved_hosts_use_the_forge_their_routes_name() {
    for (url, kind, target) in [
        (
            "https://ghe.corp.example/team/repo/pull/8",
            ForgeKind::GitHub,
            LinkKind::Pr,
        ),
        (
            "https://code.corp.example/group/sub/project/-/merge_requests/3",
            ForgeKind::GitLab,
            LinkKind::Pr,
        ),
        (
            "https://code.corp.example/group/sub/project/-/issues/4",
            ForgeKind::GitLab,
            LinkKind::Issue,
        ),
        (
            "https://code.corp.example/group/project/-/pipelines/12",
            ForgeKind::GitLab,
            LinkKind::Ci,
        ),
    ] {
        let selection = Selection::parse(Some(url.into()), None).unwrap();
        assert_eq!(selection.kind, Some(target), "{url}");
        let link = Link::parse(url).unwrap();
        assert_eq!(link.repository.kind, kind, "{url}");
        assert_eq!(
            item(target, url).unwrap().1,
            url.rsplit('/').next().unwrap().parse::<u64>().unwrap()
        );
    }
    let branch = Link::parse("https://code.corp.example/group/sub/project/-/tree/topic").unwrap();
    assert_eq!(branch.repository.kind, ForgeKind::GitLab);
    assert!(matches!(branch.target, LinkTarget::Branch(name) if name == "topic"));
}

#[test]
fn links_preserve_branch_names_and_validate_item_routes() {
    for (url, expected) in [
        (
            "https://github.com/team/repo/tree/feature/api",
            "feature/api",
        ),
        (
            "https://forge.example/team/repo/src/branch/feature%2Fapi#readme",
            "feature/api",
        ),
        (
            "http://forge.example:3000/team/repo/src/branch/topic+name",
            "topic+name",
        ),
        (
            "https://gitlab.com/group/sub/project/-/tree/feature/api",
            "feature/api",
        ),
        (
            "https://bitbucket.org/workspace/repo/branch/feature/api",
            "feature/api",
        ),
    ] {
        let link = Link::parse(url).unwrap();
        assert!(matches!(link.target, LinkTarget::Branch(branch) if branch == expected));
    }
    assert!(matches!(
        Link::parse("https://github.com/team/repo/issues/1")
            .unwrap()
            .target,
        LinkTarget::Issue
    ));
    assert!(matches!(
        Link::parse("https://forge.example/team/repo/pulls/2")
            .unwrap()
            .target,
        LinkTarget::Pr
    ));
    assert!(matches!(
        Link::parse("https://gitlab.com/group/project/-/merge_requests/2")
            .unwrap()
            .target,
        LinkTarget::Pr
    ));
    for url in [
        "https://github.com/team/repo/issues/0",
        "https://github.com/team/repo/pulls/1",
        "https://forge.example/team/repo/src/branch/%ZZ",
        "https://github.com/team/repo",
    ] {
        assert!(Link::parse(url).is_err(), "{url}");
    }
}

#[test]
fn selections_leave_bare_number_kinds_open_and_match_urls() {
    let selection = Selection::parse(Some("505".into()), None).unwrap();
    assert_eq!(selection.kind, None);
    assert_eq!(selection.input.as_deref(), Some("505"));
    assert!(Selection::parse(Some("505".into()), Some("506".into())).is_err());
    assert!(Selection::parse(Some("0".into()), None).is_err());
    let selection = Selection::parse(Some("pr".into()), Some("505".into())).unwrap();
    assert_eq!(selection.kind, Some(LinkKind::Pr));
    assert_eq!(selection.input.as_deref(), Some("505"));
    assert!(
        Selection::parse(
            Some("pr".into()),
            Some("https://github.com/team/repo/issues/1".into())
        )
        .is_err()
    );
    assert!(
        Selection::parse(Some("issue".into()), None)
            .unwrap()
            .input
            .is_none()
    );
}

#[test]
fn run_links_select_runs_by_url_or_after_ci() {
    let url = "https://forge.example/team/repo/actions/runs/2765/jobs/1";
    let link = Link::parse(url).unwrap();
    assert!(matches!(link.target, LinkTarget::Run));
    assert_eq!(
        link.repository.run(url).unwrap(),
        (
            2765,
            "https://forge.example/team/repo/actions/runs/2765".into()
        )
    );
    let selection = Selection::parse(Some(url.into()), None).unwrap();
    assert_eq!(selection.kind, Some(LinkKind::Ci));
    let selection = Selection::parse(Some("ci".into()), Some("2765".into())).unwrap();
    assert_eq!(selection.kind, Some(LinkKind::Ci));
    assert!(
        Selection::parse(
            Some("ci".into()),
            Some("https://forge.example/team/repo/pulls/2".into())
        )
        .is_err()
    );
    assert!(
        Selection::parse(Some("pr".into()), Some(url.into())).is_err(),
        "a run URL is not a PR"
    );
}

#[test]
fn an_item_url_names_its_repository_and_number_by_its_kind() -> Result<()> {
    for (kind, url) in [
        (LinkKind::Issue, "https://codeberg.org/team/repo/issues/3"),
        (LinkKind::Pr, "https://codeberg.org/team/repo/pulls/12"),
        (
            LinkKind::Ci,
            "https://codeberg.org/team/repo/actions/runs/2765",
        ),
    ] {
        let repository = ForgeRepo::from_item_url(kind, url)?;
        assert_eq!(
            repository.repository_url(),
            "https://codeberg.org/team/repo"
        );
        assert_eq!(repository.item(kind, url)?.1, url, "{kind}");
    }
    let own = ForgeRepo::parse("https://codeberg.org/team/repo")?;
    assert_eq!(own.item(LinkKind::Pr, "12")?.0, 12);
    let pull = "https://codeberg.org/team/repo/pulls/12";
    assert!(ForgeRepo::from_item_url(LinkKind::Issue, pull).is_err());
    assert!(own.item(LinkKind::Ci, pull).is_err());
    Ok(())
}

#[test]
fn item_links_of_every_forge_are_told_from_repositories() {
    for link in [
        "https://github.com/team/repo/issues/3",
        "https://github.com/team/repo/pull/4",
        "https://github.com/team/repo/tree/fix/a",
        "https://forge.example/team/repo/pulls/4",
        "https://forge.example/team/repo/src/branch/fix",
        "https://gitlab.com/group/repo/-/merge_requests/5",
        "https://gitlab.com/group/repo/-/issues/6",
        "https://bitbucket.org/team/repo/pull-requests/7",
        "https://bitbucket.org/team/repo/branch/fix",
    ] {
        assert!(is_item_link(link), "{link}");
    }
    for repository in ["https://github.com/team/repo", "my-project", "42"] {
        assert!(!is_item_link(repository), "{repository}");
    }
}

#[test]
fn an_item_url_names_its_number_in_its_own_repository_only() {
    let url = "https://github.com/team/repo/pull/4";
    assert!(names_item(
        LinkKind::Pr,
        url,
        "git@github.com:team/repo.git",
        4
    ));
    assert!(!names_item(
        LinkKind::Pr,
        url,
        "git@github.com:team/repo.git",
        5
    ));
    assert!(!names_item(
        LinkKind::Pr,
        url,
        "git@github.com:team/other.git",
        4
    ));
    assert!(!names_item(
        LinkKind::Issue,
        url,
        "git@github.com:team/repo.git",
        4
    ));
}

#[test]
fn a_branch_link_splits_at_the_route_that_leaves_a_repository() {
    for (url, repository, branch) in [
        (
            "https://github.com/team/repo/tree/feature/branch/x",
            "https://github.com/team/repo",
            "feature/branch/x",
        ),
        (
            "https://gitlab.com/group/tree/project/-/tree/fix",
            "https://gitlab.com/group/tree/project",
            "fix",
        ),
        (
            "https://gitlab.com/group/sub/tree/project/-/tree/fix",
            "https://gitlab.com/group/sub/tree/project",
            "fix",
        ),
        (
            "https://forge.example/team/repo/src/branch/fix/tree/x",
            "https://forge.example/team/repo",
            "fix/tree/x",
        ),
        (
            "https://bitbucket.org/workspace/repo/branch/fix/tree/x",
            "https://bitbucket.org/workspace/repo",
            "fix/tree/x",
        ),
    ] {
        let link = Link::parse(url).unwrap();
        assert_eq!(link.repository.repository_url(), repository, "{url}");
        assert!(
            matches!(&link.target, LinkTarget::Branch(name) if name == branch),
            "{url}"
        );
    }
}
