//! Opt-in storage benchmark: cargo test --release connection_setup -- --ignored --nocapture
use super::*;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Instant,
};

#[derive(Default)]
struct Metrics {
    opens: AtomicU64,
    setup_ns: AtomicU64,
    query_ns: AtomicU64,
    calls: AtomicU64,
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

#[derive(Clone)]
enum Backend {
    Fresh(PathBuf),
    Reuse(mpsc::SyncSender<Job>),
}

fn connect(path: &std::path::Path, metrics: &Metrics) -> Result<Connection> {
    let start = Instant::now();
    let db = Connection::open(path)?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.execute_batch("PRAGMA foreign_keys=ON;")?;
    metrics.opens.fetch_add(1, Ordering::Relaxed);
    metrics
        .setup_ns
        .fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
    Ok(db)
}

impl Backend {
    fn reuse(path: PathBuf, workers: usize, metrics: Arc<Metrics>) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Job>(128);
        let receiver = Arc::new(Mutex::new(receiver));
        for _ in 0..workers {
            let (path, receiver, metrics) = (path.clone(), receiver.clone(), metrics.clone());
            std::thread::spawn(move || {
                let mut db = connect(&path, &metrics).unwrap();
                loop {
                    let job = receiver.lock().unwrap().recv();
                    match job {
                        Ok(job) => job(&mut db),
                        Err(_) => break,
                    }
                }
            });
        }
        Self::Reuse(sender)
    }

    async fn run(
        &self,
        metrics: Arc<Metrics>,
        operation: impl FnOnce(&mut Connection) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        let setup_metrics = metrics.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        let job: Job = Box::new(move |db| {
            let start = Instant::now();
            let result = operation(db);
            metrics
                .query_ns
                .fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
            metrics.calls.fetch_add(1, Ordering::Relaxed);
            let _ = send.send(result);
        });
        match self {
            Self::Fresh(path) => {
                let path = path.clone();
                // Opening/closing and scheduling match the original Store::run.
                let metrics = setup_metrics;
                tokio::task::spawn_blocking(move || {
                    let mut db = connect(&path, &metrics).unwrap();
                    job(&mut db);
                })
                .await?;
            }
            Self::Reuse(sender) => sender
                .try_send(job)
                .map_err(|_| anyhow::anyhow!("benchmark queue full"))?,
        }
        receive.await?
    }
}

fn fixture(path: &std::path::Path, rows: usize) -> Result<()> {
    let mut db = Connection::open(path)?;
    migrate(&mut db)?;
    let tx = db.transaction()?;
    tx.execute(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1)",
        [],
    )?;
    for row in 0..rows {
        let id = row.to_string();
        tx.execute("INSERT INTO workspaces(id,repository_id,name,path,branch,state) VALUES (?1,'repo',?1,?1,?1,'ready')", [&id])?;
        tx.execute(
            "INSERT INTO executions(id,workspace_id,state) VALUES (?1,?1,'running')",
            [&id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

async fn inspection(backend: &Backend, metrics: &Arc<Metrics>, id: String) -> Result<()> {
    let selector = id.clone();
    backend
        .run(metrics.clone(), move |db| {
            let workspace = db.query_row(
                &format!("SELECT {WORKSPACE_COLUMNS} FROM workspaces WHERE id=?1"),
                [&selector],
                workspace,
            )?;
            assert_eq!(workspace.id, selector);
            Ok(())
        })
        .await?;
    backend
        .run(metrics.clone(), move |db| {
            assert_eq!(executions(db, &id)?.len(), 1);
            ports(db, Some(&id))?;
            crate::daemon::resources::leases(db, Some(&id))?;
            Ok(())
        })
        .await?;
    backend
        .run(metrics.clone(), |db| {
            let count: i64 =
                db.query_row("SELECT count(*) FROM notifications WHERE read=0", [], |r| {
                    r.get(0)
                })?;
            assert_eq!(count, 0);
            Ok(())
        })
        .await
}

async fn allocation(
    backend: &Backend,
    metrics: &Arc<Metrics>,
    id: String,
    client: usize,
) -> Result<()> {
    let owner = id.clone();
    backend
        .run(metrics.clone(), move |db| {
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            require_ready(&tx, &owner)?;
            tx.execute(
                "INSERT INTO ports(workspace_id,name,port,env_var) VALUES (?1,?2,?3,?2)",
                rusqlite::params![owner, format!("port{client}"), 20000 + client as i64],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    backend
        .run(metrics.clone(), move |db| {
            let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            assert_eq!(
                tx.execute(
                    "DELETE FROM ports WHERE workspace_id=?1 AND name=?2",
                    rusqlite::params![id, format!("port{client}")]
                )?,
                1
            );
            tx.commit()?;
            Ok(())
        })
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual release-mode measurement"]
async fn connection_setup() -> Result<()> {
    println!(
        "backend,rows,clients,workload,opens,requests_per_s,p50_us,p95_us,p99_us,setup_us_per_call,query_us_per_call"
    );
    for rows in [4, 1000] {
        for clients in [1, 16] {
            for mixed in [false, true] {
                for workers in [0, 1, 4] {
                    let temp = tempfile::tempdir_in(std::env::current_dir()?)?;
                    let path = temp.path().join("state.db");
                    fixture(&path, rows)?;
                    let metrics = Arc::new(Metrics::default());
                    let backend = if workers == 0 {
                        Backend::Fresh(path)
                    } else {
                        Backend::reuse(path, workers, metrics.clone())
                    };
                    let start = Instant::now();
                    let mut tasks = tokio::task::JoinSet::new();
                    for client in 0..clients {
                        let (backend, metrics) = (backend.clone(), metrics.clone());
                        tasks.spawn(async move {
                            let mut latencies = Vec::new();
                            for iteration in 0..200 {
                                let start = Instant::now();
                                let id = (client % rows).to_string();
                                if mixed && iteration % 5 == 0 {
                                    allocation(&backend, &metrics, id, client).await?;
                                } else {
                                    inspection(&backend, &metrics, id).await?;
                                }
                                latencies.push(start.elapsed().as_micros());
                            }
                            Ok::<_, anyhow::Error>(latencies)
                        });
                    }
                    let mut latencies = Vec::new();
                    while let Some(result) = tasks.join_next().await {
                        latencies.extend(result??);
                    }
                    let elapsed = start.elapsed().as_secs_f64();
                    latencies.sort_unstable();
                    let calls = metrics.calls.load(Ordering::Relaxed) as f64;
                    println!(
                        "{workers},{rows},{clients},{},{},{:.0},{},{},{},{:.2},{:.2}",
                        if mixed { "mixed" } else { "read" },
                        metrics.opens.load(Ordering::Relaxed),
                        latencies.len() as f64 / elapsed,
                        latencies[latencies.len() / 2],
                        latencies[latencies.len() * 95 / 100],
                        latencies[latencies.len() * 99 / 100],
                        metrics.setup_ns.load(Ordering::Relaxed) as f64 / calls / 1000.0,
                        metrics.query_ns.load(Ordering::Relaxed) as f64 / calls / 1000.0
                    );
                }
            }
        }
    }
    Ok(())
}
