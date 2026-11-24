use std::sync::Arc;

use shoal_core::test_support::{global_config, repository};
use shoal_daemon_base::forge_access::ForgeAccess;
use shoal_daemon_base::{
    config_state::ConfigState, file_access::FileAccess, gates::Gates, notifications::Notifications,
};
use shoal_forge_types::items::link::LinkKind;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    links::pr::{Registration, RegistrationKind},
    model::Relation,
};

use super::*;
use crate::stand_in::StandInRecords;

/// The links' service over a temporary store, with records that take every
/// link change.
async fn service(root: &std::path::Path) -> Arc<Links> {
    let store = shoal_store::test_support::unsynced_store(root.join("state.db"))
        .await
        .unwrap();
    let gates = Arc::new(Gates::default());
    let config = Arc::new(ConfigState::new(global_config("")));
    let forges = ForgeAccess::new(store.clone(), gates.clone(), config.clone());
    Arc::new(Links::new(
        store.clone(),
        gates,
        config,
        Arc::new(Notifications::new(store)),
        Arc::new(FileAccess::default()),
        Arc::new(forges),
        Arc::new(StandInRecords),
    ))
}

#[tokio::test]
async fn a_failed_branch_check_does_not_claim_another_branch() {
    let root = tempfile::tempdir().unwrap();
    let path = repository(root.path(), "repo");
    let workspace = shoal_protocol::test_support::workspace("workspace", "repo", "main", &path);
    current_head(&workspace).await.unwrap();
    // A worktree whose Git directory is gone, as a linked one's can be.
    std::fs::remove_dir_all(workspace.path.join(".git")).unwrap();
    std::fs::write(workspace.path.join(".git"), "gitdir: missing\n").unwrap();
    assert!(current_head(&workspace).await.is_err());
}

#[tokio::test]
async fn watching_a_related_pr_replaces_its_link_and_keeps_its_cursor() {
    let root = tempfile::tempdir().unwrap();
    let links = service(root.path()).await;
    let (id, url) = (
        SpaceId::from("workspace"),
        "https://github.com/team/repo/pull/2",
    );
    let service = links.clone();
    let counts = (links.store)
        .write(move |tx| {
            tx.execute(
                "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1)",
                [],
            )?;
            tx.execute(
                "INSERT INTO spaces(id,kind,repository_id,name) VALUES (?1,'workspace','repo','watch')",
                [&id],
            )?;
            service.record(tx, &id, Relation::Related, LinkKind::Pr, url, None)?;
            tx.execute(
                "INSERT INTO pr_activity(space_id,url,record) VALUES (?1,?2,'{}')",
                rusqlite::params![id, url],
            )?;
            let watch = Registration {
                kind: RegistrationKind::Watch {
                    urls: vec![url.parse()?],
                    merged_head: None,
                },
                error: None,
            };
            service.record_change(tx, &id, RegistrationChange::new(None, Some(watch), None))?;
            let count = |table: &str| -> rusqlite::Result<i64> {
                let query = format!("SELECT COUNT(*) FROM {table} WHERE space_id=?1");
                tx.query_row(&query, [&id], |row| row.get(0))
            };
            Ok((count("space_items")?, count("pr_activity")?))
        })
        .await
        .unwrap();
    assert_eq!(counts, (0, 1));
}
