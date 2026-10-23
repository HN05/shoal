//! The CLI side of `config show`: the daemon's repository layers under the
//! global file read now.
use anyhow::Result;

use crate::{
    cli::client::request,
    config::{Config, repo::ConfigLayers, resolve::Stack},
    paths::Paths,
    protocol::{ConfigTarget, Method},
};

pub use crate::config::resolve::Entry;

/// Every option with its winning layer, and warnings for retired settings.
pub async fn load(paths: &Paths, target: ConfigTarget) -> Result<(Vec<Entry>, Vec<String>)> {
    let layers = request::<Box<ConfigLayers>>(paths, Method::LayeredConfig { target }).await?;
    let stack = Stack::new(&Config::load_with_templates(paths)?, &layers);
    Ok((stack.report()?, stack.warnings()))
}
