use super::*;

#[test]
fn a_pager_setting_is_a_boolean_as_git_reads_one_or_a_command() {
    for on in ["true", "Yes", "on", "1", "-2"] {
        assert_eq!(Paging::of(on), Paging::On, "{on}");
    }
    for off in ["false", "NO", "off", "0"] {
        assert_eq!(Paging::of(off), Paging::Off, "{off}");
    }
    assert_eq!(Paging::of("less -S"), Paging::Command("less -S"));
}

#[tokio::test]
async fn a_repository_that_turns_off_the_diff_s_pager_pages_nothing() {
    use crate::test_support::{git, repository};
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path(), "repo");
    git(&repo, &["config", "pager.diff", "false"]);
    assert_eq!(diff_pager(&repo).await.unwrap(), None);
}
