use std::{
    collections::HashSet,
    pin::Pin,
    sync::{Arc, Mutex},
};

use anyhow::{Result, anyhow};
use tokio::{sync::Semaphore, task::JoinHandle};

use super::*;

type BoxedScan = Pin<Box<dyn Future<Output = Result<Scan>> + Send>>;

/// A scan that records the executions it covers and waits for a permit of
/// `gate` before finishing.
#[derive(Clone)]
struct Stub {
    covered: Arc<Mutex<Vec<HashSet<String>>>>,
    gate: Arc<Semaphore>,
}

impl Default for Stub {
    fn default() -> Self {
        Self {
            covered: Arc::default(),
            gate: Arc::new(Semaphore::new(0)),
        }
    }
}

impl Stub {
    fn scan(&self) -> impl Fn(HashSet<String>) -> BoxedScan + Send + 'static {
        let stub = self.clone();
        move |ids| {
            let stub = stub.clone();
            Box::pin(async move {
                stub.covered.lock().unwrap().push(ids);
                let _permit = stub.gate.acquire().await?;
                Ok(Scan::default())
            })
        }
    }

    fn covered(&self) -> Vec<HashSet<String>> {
        self.covered.lock().unwrap().clone()
    }

    /// Wait until `count` scans started.
    async fn started(&self, count: usize) {
        while self.covered.lock().unwrap().len() < count {
            tokio::task::yield_now().await;
        }
    }
}

fn ids(names: &[&str]) -> HashSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn check(scans: &Arc<SharedScans>, stub: &Stub, names: &[&str]) -> JoinHandle<Result<Arc<Scan>>> {
    let (scans, scan, ids) = (scans.clone(), stub.scan(), ids(names));
    tokio::spawn(async move { scans.scan_with(ids, scan).await })
}

/// Wait until the next scan covers `count` executions.
async fn joined(scans: &SharedScans, count: usize) {
    let waiting = || {
        let state = scans.state.lock().unwrap();
        state.next.as_ref().map_or(0, |batch| batch.ids.len())
    };
    while waiting() < count {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn checks_made_during_a_scan_share_the_next_one() {
    let scans = Arc::new(SharedScans::default());
    let stub = Stub::default();
    let first = check(&scans, &stub, &["a"]);
    stub.started(1).await;
    // Both ask while the first scan runs, so they wait for a new one.
    let b = check(&scans, &stub, &["b"]);
    let c = check(&scans, &stub, &["c", "a"]);
    joined(&scans, 3).await;
    stub.gate.add_permits(2);
    first.await.unwrap().unwrap();
    let (b, c) = (b.await.unwrap().unwrap(), c.await.unwrap().unwrap());
    assert!(Arc::ptr_eq(&b, &c));
    assert_eq!(stub.covered(), vec![ids(&["a"]), ids(&["a", "b", "c"])]);
}

#[tokio::test]
async fn a_scan_outlives_the_check_that_started_it() {
    let scans = Arc::new(SharedScans::default());
    let stub = Stub::default();
    let first = check(&scans, &stub, &["a"]);
    stub.started(1).await;
    first.abort();
    let second = check(&scans, &stub, &["b"]);
    joined(&scans, 1).await;
    stub.gate.add_permits(2);
    second.await.unwrap().unwrap();
    assert_eq!(stub.covered(), vec![ids(&["a"]), ids(&["b"])]);
}

#[tokio::test]
async fn a_failed_scan_fails_every_check_it_covers() {
    let scans = SharedScans::default();
    let failing = |_| async { Err::<Scan, _>(anyhow!("ps failed")) };
    let (a, b) = tokio::join!(
        scans.scan_with(ids(&["a"]), failing),
        scans.scan_with(ids(&["b"]), failing),
    );
    assert!(a.is_err() && b.is_err());
    // A later check starts a scan of its own.
    let stub = Stub::default();
    stub.gate.add_permits(1);
    scans.scan_with(ids(&["c"]), stub.scan()).await.unwrap();
    assert_eq!(stub.covered(), vec![ids(&["c"])]);
}
