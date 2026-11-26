//! Requests about the files attached to a space.
use std::path::PathBuf;

use anyhow::Result;
use shoal_protocol::files::{AttachedFile, FetchedDirectory, NewFile};
use shoal_protocol::protocol::{Body, FileMethod, Responses};
use shoal_protocol::space_names::SpaceSelector;
use tokio::{io::BufReader, net::UnixStream};

use crate::connection::{Daemon, PreparedPages, open_buffered, request};

pub fn list(space: SpaceSelector) -> PreparedPages<AttachedFile> {
    PreparedPages::new(FileMethod::ListFiles { space })
}

pub fn add(space: SpaceSelector, files: Vec<NewFile>) -> PreparedPages<AttachedFile> {
    PreparedPages::new(FileMethod::AddFiles { space, files })
}

/// Delete the files of `space`, or of every space the caller reaches, that
/// `names` and `ids` select.
pub fn remove(
    space: Option<SpaceSelector>,
    names: Vec<String>,
    ids: Vec<i64>,
) -> PreparedPages<AttachedFile> {
    PreparedPages::new(FileMethod::RemoveFiles { space, names, ids })
}

/// The directory, marked as Shoal's, where the caller's machine keeps the
/// copies it fetches of `space`'s files from the machine that keeps them.
pub async fn fetched_directory(daemon: &impl Daemon, space: SpaceSelector) -> Result<PathBuf> {
    let method = FileMethod::FetchedDirectory { space };
    let directory: FetchedDirectory = request(daemon, method).await?;
    Ok(directory.path)
}

/// An attached file's bytes, as the machine that keeps its copy reads them.
pub struct FileRead {
    responses: Responses<BufReader<UnixStream>>,
    first: Option<Body>,
}

/// Read the file `id` attached to `space`, on the machine that keeps its
/// copy. Fails at once when the daemon refuses, as for a file outside the
/// caller's scope or one kept on a machine that is offline.
pub async fn read(daemon: &impl Daemon, space: SpaceSelector, id: i64) -> Result<FileRead> {
    let (stream, first) = open_buffered(daemon, FileMethod::ReadFile { space, id }).await?;
    Ok(FileRead {
        responses: Responses::new(stream),
        first: Some(first.into_result()?),
    })
}

impl FileRead {
    /// The next bytes of the file; none once it has all come.
    pub async fn next(&mut self) -> Result<Option<Vec<u8>>> {
        let body = match self.first.take() {
            Some(body) => body,
            None => self.responses.next().await?.body.into_result()?,
        };
        match body {
            Body::FileBytes(bytes) => bytes.bytes(),
            body => Err(body.unexpected("FileBytes")),
        }
    }

    /// The rest of the file's bytes.
    pub async fn to_end(mut self) -> Result<Vec<u8>> {
        let mut all = Vec::new();
        while let Some(bytes) = self.next().await? {
            all.extend(bytes);
        }
        Ok(all)
    }
}

#[cfg(test)]
mod tests;
