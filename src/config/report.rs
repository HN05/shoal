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

pub async fn load(paths: &Paths, target: ConfigTarget) -> Result<Vec<Entry>> {
    let layers = request::<Box<ConfigLayers>>(paths, Method::LayeredConfig { target }).await?;
    Stack::new(&Config::load(paths)?, &layers).report()
}
