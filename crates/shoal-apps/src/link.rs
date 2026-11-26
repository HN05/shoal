//! The hub's answers to its members' app store requests, as the frames its
//! socket would answer its own callers with.
use anyhow::Result;
use shoal_protocol::{
    apps::store::EntryPiece,
    protocol::{AppMethod, Body, Response},
};

use crate::{Apps, Holder};

impl Apps {
    /// The answer to `method`, other than a watch, of `holder`'s entries,
    /// a write of `bytes`.
    pub async fn answer_kept(
        &self,
        holder: Holder,
        method: AppMethod,
        bytes: Option<Vec<u8>>,
    ) -> Result<Vec<Body>> {
        Ok(match method {
            AppMethod::ReadAppEntry { app, key, .. } => {
                let stored = self.read_entry(holder, app, key).await?;
                vec![Body::AppEntryPieces(
                    stored.map(EntryPiece::split).unwrap_or_default(),
                )]
            }
            AppMethod::ListAppEntries { app, prefix, .. } => {
                let listed = self.list_entries(holder, app, prefix).await?;
                vec![Body::AppEntries(listed), Body::Ok]
            }
            // Without its bytes, a write is checked before they come.
            AppMethod::WriteAppEntry {
                app, write, size, ..
            } => match bytes {
                Some(bytes) => vec![Body::AppEntry(
                    self.write_entry(holder, app, write, bytes).await?,
                )],
                None => {
                    self.check_write(holder, app, write, size).await?;
                    vec![Body::Ok]
                }
            },
            AppMethod::DeleteAppEntry {
                app, key, replaces, ..
            } => {
                self.delete_entry(holder, app, key, replaces).await?;
                vec![Body::Ok]
            }
            AppMethod::ReadAppConfig { .. } | AppMethod::WatchAppEntries { .. } => {
                anyhow::bail!("not a request of kept entries")
            }
        })
    }
}

/// The frames of `body`, one per line, as the socket writes them.
pub fn frames(body: Body) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for frame in Response::new(0, body).frames()? {
        serde_json::to_writer(&mut bytes, &frame)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}
