use super::*;

#[tokio::test]
async fn reuses_connection_and_preserves_sql_errors() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = Store::open(root.path().join("state.db")).await?;
    store
        .run(|db| {
            assert_eq!(
                db.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                db.query_row("PRAGMA busy_timeout", [], |r| r.get::<_, i64>(0))?,
                5000
            );
            db.execute_batch("CREATE TEMP TABLE reused(value); INSERT INTO reused VALUES (42);")?;
            Ok(())
        })
        .await?;
    assert_eq!(
        store
            .run(|db| Ok(db.query_row("SELECT value FROM reused", [], |r| r.get::<_, i64>(0))?))
            .await?,
        42
    );
    let error = store.run(|db| {
        db.execute("INSERT INTO ports(workspace_id,name,port,env_var) VALUES ('missing','web',1234,'PORT')", [])?;
        Ok(())
    }).await.unwrap_err();
    assert!(
        matches!(error.downcast_ref::<rusqlite::Error>(), Some(rusqlite::Error::SqliteFailure(code, _)) if code.code == rusqlite::ErrorCode::ConstraintViolation)
    );
    store.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn failures_are_never_replayed_and_unfinished_transactions_are_discarded() -> Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("state.db");
    let store = Store::open(path.clone()).await?;
    store
        .run(|db| {
            db.execute_batch("CREATE TABLE effects(value);")?;
            Ok(())
        })
        .await?;
    for panic in [false, true] {
        let error = store
            .run(move |db| -> Result<()> {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                tx.execute("INSERT INTO effects VALUES (1)", [])?;
                tx.commit()?;
                assert!(!panic, "panic after commit");
                anyhow::bail!("error after commit")
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains(if panic {
            "database worker failed"
        } else {
            "error after commit"
        }));
    }
    assert!(
        store
            .run(|db| -> Result<()> {
                db.execute_batch("BEGIN IMMEDIATE; INSERT INTO effects VALUES (99);")?;
                anyhow::bail!("unfinished transaction")
            })
            .await
            .is_err()
    );
    store
        .run(|db| {
            assert!(db.is_autocommit());
            assert_eq!(
                db.query_row("SELECT sum(value) FROM effects", [], |r| r.get::<_, i64>(0))?,
                2
            );
            assert_eq!(
                db.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .await?;
    store.shutdown().await;
    let reopened = Store::open(path).await?;
    assert_eq!(
        reopened
            .run(|db| Ok(db.query_row("SELECT count(*) FROM effects", [], |r| r.get::<_, i64>(0))?))
            .await?,
        2
    );
    reopened.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn full_queue_waits_and_drains_cancelled_callers_before_shutdown() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = Store::open(root.path().join("state.db")).await?;
    let (entered, waiting) = oneshot::channel();
    let (release, held) = std::sync::mpsc::channel();
    let clone = store.clone();
    let active = tokio::spawn(async move {
        clone
            .run(move |db| {
                let _ = entered.send(());
                held.recv()?;
                db.execute_batch("CREATE TABLE drained(value); INSERT INTO drained VALUES (1);")?;
                Ok(())
            })
            .await
    });
    waiting.await?;
    let mut queued = Vec::new();
    // Submit directly to make saturation deterministic without sleeps.
    for _ in 0..QUEUE_CAPACITY {
        let (send, receive) = oneshot::channel();
        store
            .worker
            .sender
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .try_send(Box::new(move |_, _| {
                let _ = send.send(());
            }))
            .unwrap();
        queued.push(receive);
    }
    let clone = store.clone();
    let (ran, cancelled) = std::sync::mpsc::channel::<()>();
    let mut cancelled_caller = tokio::spawn(async move {
        clone
            .run(move |_| {
                let _ = ran.send(());
                Ok(())
            })
            .await
    });
    let clone = store.clone();
    let mut waiting_caller = tokio::spawn(async move { clone.run(|_| Ok(())).await });
    for caller in [&mut cancelled_caller, &mut waiting_caller] {
        assert!(
            tokio::time::timeout(Duration::from_millis(20), caller)
                .await
                .is_err()
        );
    }
    // Cancelling before admission drops the operation without running it.
    cancelled_caller.abort();
    active.abort();
    let clone = store.clone();
    let mut shutdown = tokio::spawn(async move { clone.shutdown().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut shutdown)
            .await
            .is_err()
    );
    release.send(())?;
    shutdown.await?;
    for receive in queued {
        receive.await?;
    }
    waiting_caller.await??;
    assert_eq!(
        cancelled.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Disconnected)
    );
    assert!(
        store
            .run(|_| Ok(()))
            .await
            .unwrap_err()
            .to_string()
            .contains("shut down")
    );
    store.shutdown().await;
    let reopened = Store::open(root.path().join("state.db")).await?;
    assert_eq!(
        reopened
            .run(|db| Ok(db.query_row("SELECT value FROM drained", [], |r| r.get::<_, i64>(0))?))
            .await?,
        1
    );
    reopened.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn external_contention_keeps_the_timeout_and_runtime_responsive() -> Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("state.db");
    let store = Store::open(path.clone()).await?;
    let (locked, waiting) = oneshot::channel();
    let (release, held) = std::sync::mpsc::channel();
    let external = tokio::task::spawn_blocking(move || -> Result<()> {
        let mut db = Connection::open(path)?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let _ = locked.send(());
        held.recv()?;
        tx.rollback()?;
        Ok(())
    });
    waiting.await?;
    let clone = store.clone();
    let mut claim = tokio::spawn(async move {
        clone
            .run(|db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                tx.commit()?;
                Ok(())
            })
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut claim)
            .await
            .is_err()
    );
    let error = claim.await?.unwrap_err();
    assert!(
        matches!(error.downcast_ref::<rusqlite::Error>(), Some(rusqlite::Error::SqliteFailure(code, _)) if code.code == rusqlite::ErrorCode::DatabaseBusy)
    );
    release.send(())?;
    external.await??;
    store
        .run(|db| {
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    store.shutdown().await;
    Ok(())
}
