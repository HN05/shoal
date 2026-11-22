//! The local reviews an execution may queue.
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Reviews;

use crate::daemon::manager::Manager;

impl Reviews for Manager {
    fn queue_unreviewed(&self, refusal: anyhow::Error) -> BoxFuture<'_, anyhow::Error> {
        Box::pin(async move {
            match self
                .reviews
                .queue_unreviewed::<()>(self, Err(refusal))
                .await
            {
                Err(error) => error,
                Ok(()) => unreachable!("a refusal stays one"),
            }
        })
    }
}
