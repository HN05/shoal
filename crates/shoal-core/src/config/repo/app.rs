//! The `[app]` section: the desktop app Shoal opens.
use crate::config::options::Options;
use shoal_states::states;

use serde::{Deserialize, Serialize};

states!(
    DesktopApp: ValueEnum {
        Claude => "claude",
        Codex => "codex",
        T3 => "t3",
    }
);

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = App)]
pub struct AppConfig {
    pub default: Option<DesktopApp>,
}
