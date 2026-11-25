use super::*;
use shoal_store::test_support::queries::{check_query_growth, populated_database};

#[test]
fn repository_space_reads_search_their_index() -> Result<()> {
    let db = populated_database()?;
    // A missing repository exercises the entire lookup even with EXISTS short-circuiting.
    check_query_growth(
        &db,
        &format!("SELECT EXISTS({REPOSITORY_SPACES_QUERY})"),
        "spaces",
        "repository_id",
        "missing",
    )?;
    assert!(!repository_has_spaces(&db, &"missing".into())?);
    assert!(repository_has_spaces(&db, &"owner-0".into())?);
    Ok(())
}
