use serde::{Deserialize, Serialize};

use super::{ConfigTarget, Lifetime};
use crate::apps::AppName;

/// Requests of apps built on Shoal.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppMethod {
    /// The table `[apps.<app>]` resolves to for `target` after every
    /// layer, none where no layer sets it. Answers with
    /// [`super::Body::AppConfig`].
    ReadAppConfig { app: AppName, target: ConfigTarget },
}

impl AppMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::ReadAppConfig { .. } => Lifetime::Reply,
        }
    }
}
