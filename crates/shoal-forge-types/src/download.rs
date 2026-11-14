//! A file the forge serves on its web host, such as an image in a
//! description, as Shoal downloaded it.

/// A downloaded file and the content type the forge served it as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}
