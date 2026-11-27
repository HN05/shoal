use super::*;
use crate::stand_in::Fixture;
use serde_json::json;

fn to_workspace() -> ToWorkspace {
    ToWorkspace {
        mode: crate::model::chat_mode(),
        chat: serde_json::from_value(json!({"scope_token": "token", "id": "w", "mode": "chat"}))
            .unwrap(),
        execution: ExecutionId::generate(),
        preparation: Some("operation".into()),
        prepared: false,
        hook_failure: None,
        carrying: Carrying::default(),
    }
}

fn to_chat() -> ToChat {
    ToChat {
        chat: Box::new(ConvertedChat {
            agent: "claude".into(),
            execution: ExecutionId::generate(),
            origin: None,
            mode: crate::model::chat_mode(),
            narrowing: Default::default(),
        }),
        held: false,
        carrying: Carrying::default(),
    }
}

fn to_agent() -> ToAgent {
    ToAgent {
        execution: ExecutionId::generate(),
        taking: Box::new(Taking {
            agent: "codex".into(),
            mode: crate::model::chat_mode(),
            model: Default::default(),
            narrowing: Default::default(),
            note: Some("carry on".into()),
        }),
        held: false,
    }
}

/// What a conversion holds, comparable: its target, preparation and the
/// rest as JSON.
fn shown(pending: &Pending) -> (ConversionTarget, Option<String>, serde_json::Value) {
    let (target, preparation, record) = pending.columns().unwrap();
    (
        target,
        preparation.map(str::to_owned),
        serde_json::from_str(&record).unwrap(),
    )
}

/// `pending` after recording it as space `id`'s conversion in `store`,
/// `edit` and reading it back, with the target and preparation its columns
/// hold.
async fn stored_in(
    store: &shoal_store::Store,
    id: &str,
    pending: Pending,
    edit: impl FnOnce(&Transaction<'_>, &SpaceId) -> Result<()> + Send + 'static,
) -> (Pending, (ConversionTarget, Option<String>)) {
    let id = SpaceId::from(id);
    store
        .write(move |tx| {
            record(tx, &id, &pending)?;
            edit(tx, &id)?;
            let columns = tx.query_row(
                "SELECT target,preparation FROM space_conversions WHERE space_id=?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            Ok((super::pending(tx, &id)?.unwrap(), columns))
        })
        .await
        .unwrap()
}

async fn stored(
    pending: Pending,
    edit: impl FnOnce(&Transaction<'_>, &SpaceId) -> Result<()> + Send + 'static,
) -> (Pending, (ConversionTarget, Option<String>)) {
    let manager = Fixture::new().await;
    stored_in(&manager.store, "w", pending, edit).await
}

#[tokio::test]
async fn each_conversion_round_trips_through_its_columns_and_record() {
    let manager = Fixture::new().await;
    for (id, pending) in [
        ("w", Pending::Workspace(to_workspace())),
        ("c", Pending::Chat(to_chat())),
        ("a", Pending::Agent(to_agent())),
    ] {
        let expected = shown(&pending);
        let (read, columns) = stored_in(&manager.store, id, pending, |_, _| Ok(())).await;
        assert_eq!(shown(&read), expected);
        assert_eq!(columns, (expected.0, expected.1));
    }
}

#[tokio::test]
async fn a_preparation_s_end_keeps_its_preparation_and_records_the_hook_failure() {
    let (read, columns) = stored(Pending::Workspace(to_workspace()), |tx, id| {
        record_prepared(tx, id, "operation", Some("hook exited 1"))
    })
    .await;
    let Pending::Workspace(to) = read else {
        panic!("expected the workspace's conversion: {read:?}");
    };
    assert!(to.prepared);
    assert_eq!(to.hook_failure.as_deref(), Some("hook exited 1"));
    assert_eq!(
        columns,
        (ConversionTarget::Workspace, Some("operation".into()))
    );
}

#[tokio::test]
async fn the_end_of_another_preparation_changes_nothing() {
    let (read, _) = stored(Pending::Workspace(to_workspace()), |tx, id| {
        record_prepared(tx, id, "other", None)
    })
    .await;
    let Pending::Workspace(to) = read else {
        panic!("expected the workspace's conversion: {read:?}");
    };
    assert!(!to.prepared);
    assert_eq!(to.preparation.as_deref(), Some("operation"));
}

#[tokio::test]
async fn a_hold_keeps_the_agent_that_carries_on_stopped() {
    let (read, _) = stored(Pending::Agent(to_agent()), hold).await;
    assert!(!read.restarts_agent());
}
