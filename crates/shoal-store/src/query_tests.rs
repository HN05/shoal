use super::*;
use crate::test_support::queries::{check_owner_query, check_query_growth, populated_database};
use shoal_ids::space_ids::SpaceId;

#[test]
fn owner_port_reads_search_existing_index() -> Result<()> {
    check_owner_query(&ports_query(true), "ports", "workspace_id")
}

#[test]
fn port_lists_preserve_global_order_and_owner_selection() -> Result<()> {
    let db = populated_database()?;
    let all_ports = ports(&db, None)?;
    assert_eq!(all_ports.len(), 20);
    assert!(all_ports.windows(2).all(
        |pair| (&pair[0].workspace_id, &pair[0].name) < (&pair[1].workspace_id, &pair[1].name)
    ));
    for owner in ["owner-0", "owner-9", "missing", ""] {
        let expected_ports: Vec<_> = all_ports
            .iter()
            .filter(|p| p.workspace_id == owner)
            .collect();
        assert_eq!(
            serde_json::to_value(ports(&db, Some(&SpaceId::from(owner)))?)?,
            serde_json::to_value(expected_ports)?
        );
    }
    Ok(())
}

#[test]
fn port_lists_propagate_decoding_and_prepare_errors() -> Result<()> {
    let db = populated_database()?;
    db.execute_batch(
        "PRAGMA ignore_check_constraints=ON;
        UPDATE ports SET port=-port WHERE workspace_id='owner-0';",
    )?;
    for owner in [None, Some(SpaceId::from("owner-0"))] {
        assert!(ports(&db, owner.as_ref()).is_err());
    }
    assert_eq!(ports(&db, Some(&SpaceId::from("owner-1")))?.len(), 2);
    db.execute_batch("DROP TABLE ports;")?;
    for owner in [None, Some(SpaceId::from("owner-0"))] {
        assert!(ports(&db, owner.as_ref()).is_err());
    }
    Ok(())
}

#[test]
fn execution_owner_reads_search_their_index() -> Result<()> {
    let db = populated_database()?;
    check_query_growth(
        &db,
        &executions_query(),
        "executions",
        "space_id",
        "owner-0",
    )
}

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
