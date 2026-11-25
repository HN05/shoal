use anyhow::Result;
use shoal_ids::space_ids::SpaceId;

use super::*;
use crate::test_support::queries::{check_owner_query, populated_database};
use crate::test_support::rows::{check_columns, mapped_rows};

#[test]
fn the_port_mapper_uses_named_columns() -> Result<()> {
    check_columns(
        &mapped_rows()?,
        "ports",
        PORT_COLUMNS,
        port,
        serde_json::json!({
            "workspace_id": "workspace", "name": "web", "port": 12345,
            "env_var": "WEB_PORT", "reason": null
        }),
    )
}

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
