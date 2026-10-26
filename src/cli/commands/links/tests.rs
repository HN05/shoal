use super::is_item_link;

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
