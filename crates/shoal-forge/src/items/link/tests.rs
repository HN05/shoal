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
