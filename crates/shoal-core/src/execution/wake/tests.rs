use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

#[tokio::test(start_paused = true)]
async fn a_failed_prompt_is_retried_while_it_stays_due() {
    let sent = AtomicUsize::new(0);
    let prompting = prompt_when_due(
        // Prompting stays due; once taken, the agent never idles again.
        |due| async move {
            if !due {
                std::future::pending::<()>().await;
            }
        },
        || async {
            match sent.fetch_add(1, Ordering::SeqCst) {
                0 => anyhow::bail!("zmx is not running"),
                _ => Ok(()),
            }
        },
    );
    // A hang guard only: the paused clock skips the retry interval.
    let _ = tokio::time::timeout(RETRY_INTERVAL * 10, prompting).await;
    assert_eq!(sent.load(Ordering::SeqCst), 2);
}
