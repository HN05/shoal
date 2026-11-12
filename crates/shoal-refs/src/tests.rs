use super::{BranchName, CommitId};

#[test]
fn commits_and_branches_are_json_strings_and_sqlite_text() -> anyhow::Result<()> {
    let commit = CommitId::from("0123456789abcdef0123456789abcdef01234567");
    let branch = BranchName::from("feature/login");
    assert_eq!(
        serde_json::to_string(&(&commit, &branch))?,
        r#"["0123456789abcdef0123456789abcdef01234567","feature/login"]"#
    );
    assert_eq!(
        serde_json::from_str::<(CommitId, BranchName)>(&serde_json::to_string(&(
            &commit, &branch
        ))?)?,
        (commit.clone(), branch.clone())
    );
    #[cfg(feature = "rusqlite")]
    {
        let db = rusqlite::Connection::open_in_memory()?;
        let (stored, kind): (CommitId, String) =
            db.query_row("SELECT ?1, typeof(?1)", [&commit], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        assert_eq!((stored, kind.as_str()), (commit, "text"));
        let stored: BranchName = db.query_row("SELECT ?1", [&branch], |row| row.get(0))?;
        assert_eq!(stored, branch);
    }
    Ok(())
}

#[test]
fn a_short_commit_is_its_first_twelve_characters() {
    assert_eq!(CommitId::from("0123456789abcdef").short(), "0123456789ab");
    assert_eq!(CommitId::from("abc").short(), "abc");
}
