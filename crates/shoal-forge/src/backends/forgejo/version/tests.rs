use super::*;

#[test]
fn forgejo_versions_name_their_release_and_gitea_versions_none() {
    assert_eq!(forgejo_release("17.0.1+gitea-1.22.0"), Some(17));
    assert_eq!(forgejo_release("16.0.0+gitea-1.22.0"), Some(16));
    assert_eq!(
        forgejo_release("18.0.0-dev-42-abc123+gitea-1.22.0"),
        Some(18)
    );
    assert_eq!(forgejo_release("1.25.0"), None);
    assert_eq!(forgejo_release("1.26.0+dev-12-gabc123"), None);
    assert_eq!(forgejo_release(""), None);
}
