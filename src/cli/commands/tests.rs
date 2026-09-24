use super::*;
use std::{cell::Cell, future::Future, task::Poll, time::Duration};
use tokio::sync::{Semaphore, mpsc};

fn workspaces(count: usize) -> Vec<Workspace> {
    (0..count)
        .map(|index| {
            Workspace::new_record(
                "repo".into(),
                index.to_string(),
                "/unused".into(),
                index.to_string(),
                crate::state::WorkspaceState::Ready,
            )
        })
        .collect()
}

struct InFlight<'a>(&'a Cell<usize>);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

#[tokio::test]
async fn workspace_overviews_refill_past_delays_and_failures_in_workspace_order() {
    let limit = WORKSPACE_OVERVIEW_CONCURRENCY;
    let workspaces = workspaces(limit * 3);
    let gates: Vec<_> = workspaces.iter().map(|_| Semaphore::new(0)).collect();
    let active = Cell::new(0);
    let peak = Cell::new(0);
    let (started, mut starts) = mpsc::unbounded_channel();
    let collect = collect_workspace_overviews(&workspaces, async |workspace| {
        let index: usize = workspace.name.parse().unwrap();
        active.set(active.get() + 1);
        peak.set(peak.get().max(active.get()));
        let _active = InFlight(&active);
        started.send(index).unwrap();
        gates[index].acquire().await.unwrap().forget();
        if index == 1 {
            anyhow::bail!("controlled failure");
        }
        Ok(index)
    });
    let release = async {
        for index in 0..limit {
            assert_eq!(starts.recv().await, Some(index));
        }
        assert!(starts.try_recv().is_err());
        // Keep the first request pending while failures and later work free slots.
        for index in limit..workspaces.len() {
            gates[index - limit + 1].add_permits(1);
            assert_eq!(starts.recv().await, Some(index));
        }
        for gate in &gates {
            gate.add_permits(1);
        }
    };
    let (overviews, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(collect, release)
    })
    .await
    .expect("a delayed workspace blocked collection");
    assert_eq!(peak.get(), limit);
    assert_eq!(active.get(), 0);
    assert_eq!(overviews.len(), workspaces.len());
    for (index, result) in overviews.into_iter().enumerate() {
        match result {
            WorkspaceOverviewResult::Ready(value) => {
                assert_ne!(index, 1);
                assert_eq!(value, index);
            }
            WorkspaceOverviewResult::Failed { workspace, error } => {
                assert_eq!(index, 1);
                assert_eq!(workspace.id, workspaces[index].id);
                assert_eq!(error, "controlled failure");
            }
        }
    }
}

#[tokio::test]
async fn cancelling_workspace_overviews_drops_in_flight_requests() {
    let workspaces = workspaces(WORKSPACE_OVERVIEW_CONCURRENCY * 2);
    let active = Cell::new(0);
    let started = Cell::new(0);
    let mut collect = Box::pin(collect_workspace_overviews(&workspaces, async |_| {
        active.set(active.get() + 1);
        started.set(started.get() + 1);
        let _active = InFlight(&active);
        std::future::pending::<Result<()>>().await
    }));
    std::future::poll_fn(|cx| {
        assert!(collect.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(active.get(), WORKSPACE_OVERVIEW_CONCURRENCY);
    drop(collect);
    assert_eq!(active.get(), 0);
    assert_eq!(started.get(), WORKSPACE_OVERVIEW_CONCURRENCY);
}

#[tokio::test]
async fn empty_workspace_overviews_make_no_requests() {
    let overviews =
        collect_workspace_overviews::<()>(&[], async |_| panic!("empty collection made a request"))
            .await;
    assert!(overviews.is_empty());
}

#[tokio::test]
async fn workspace_overviews_account_for_request_timeouts() {
    let workspaces = workspaces(WORKSPACE_OVERVIEW_CONCURRENCY * 2);
    let overviews = collect_workspace_overviews(&workspaces, async |workspace| {
        if workspace.name == "0" {
            tokio::time::timeout(Duration::from_millis(1), std::future::pending::<()>())
                .await
                .context("daemon request timed out")?;
        }
        Ok(workspace.name.clone())
    })
    .await;
    assert_eq!(overviews.len(), workspaces.len());
    let WorkspaceOverviewResult::Failed { error, .. } = &overviews[0] else {
        panic!("timeout was not reported");
    };
    assert!(error.starts_with("daemon request timed out:"));
    assert!(overviews[1..].iter().all(|result| !result.is_failed()));
}
