//! Merging: how `shoal merge` merges a space's work, and what a local merge
//! of a workspace's branch would do.
use anyhow::Result;

use crate::connection::{Daemon, Prepared, request};
use shoal_protocol::{
    model::{MergeOptions, MergePath, MergePreview},
    protocol::MergingMethod,
    space_names::SpaceSelector,
};

/// How `shoal merge` merges the work of `space`: through its one open PR,
/// or a workspace's branch locally.
pub fn merge_path(space: SpaceSelector) -> Prepared<MergePath> {
    Prepared::new(MergingMethod::MergePath { space })
}

/// What merging `workspace`'s branch as `options` say would do.
pub async fn preview_merge(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    options: &MergeOptions,
) -> Result<MergePreview> {
    let preview = MergingMethod::PreviewMerge {
        workspace,
        push: options.push,
        method: options.method,
        message: options.message.clone(),
    };
    request(daemon, preview).await
}
