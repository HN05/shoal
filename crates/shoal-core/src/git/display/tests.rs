use super::*;

#[tokio::test]
async fn a_repository_that_turns_off_the_diff_s_pager_pages_nothing() {
    use crate::test_support::{git, repository};
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    // Git reads an empty value as false.
    for off in ["false", "", "0"] {
        git(&repo, &["config", "pager.diff", off]);
        assert_eq!(diff_pager(&repo).await.unwrap(), None, "{off:?}");
    }
}

#[tokio::test]
async fn a_diff_pager_setting_is_a_boolean_where_git_reads_one_else_a_command() {
    use crate::test_support::{git, repository};
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["config", "pager.diff", "yes"]);
    assert!(matches!(diff_paging(&repo).await.unwrap(), Paging::Default));
    git(&repo, &["config", "pager.diff", "less -S"]);
    let paging = diff_paging(&repo).await.unwrap();
    assert!(matches!(&paging, Paging::Command(command) if command == "less -S"));
}
