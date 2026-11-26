//! The `[terminal]` section and each terminal host's own.
use crate::config::options::Options;
use crate::sessions::HostName;
use shoal_states::states;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Terminal)]
pub struct TerminalConfig {
    #[option(default = DefaultHost::default(), resolve = DefaultHost::resolve -> HostName)]
    pub default_host: Option<DefaultHost>,
    pub focus: Option<bool>,
    #[option(default = true)]
    pub close_when_done: Option<bool>,
    #[option(default = false)]
    pub nested_zmx: Option<bool>,
    #[option(section)]
    pub herdr: HerdrConfig,
    #[option(section)]
    pub rex: RexConfig,
    #[option(section)]
    pub tmux: TmuxConfig,
    #[option(section)]
    pub zellij: ZellijConfig,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Herdr)]
pub struct HerdrConfig {
    #[option(default = true)]
    pub enabled: Option<bool>,
    pub tab_name: Option<String>,
    #[option(default = true)]
    pub new_tab: Option<bool>,
    #[option(default = "{repo}".into())]
    pub workspace_name: Option<String>,
    pub close_when_done: Option<bool>,
    pub nested_zmx: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Rex)]
pub struct RexConfig {
    #[option(default = true)]
    pub enabled: Option<bool>,
    #[option(default = true)]
    pub new_window: Option<bool>,
    #[option(default = Placement::default())]
    pub placement: Option<Placement>,
    pub window_name: Option<String>,
    #[option(default = "{repo}".into())]
    pub session_name: Option<String>,
    pub close_when_done: Option<bool>,
    pub nested_zmx: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Tmux)]
pub struct TmuxConfig {
    pub enabled: Option<bool>,
    #[option(default = true)]
    pub new_window: Option<bool>,
    #[option(default = Placement::default())]
    pub placement: Option<Placement>,
    pub window_name: Option<String>,
    #[option(default = "{repo}".into())]
    pub session_name: Option<String>,
    pub close_when_done: Option<bool>,
    pub nested_zmx: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Zellij)]
pub struct ZellijConfig {
    pub enabled: Option<bool>,
    #[option(default = true)]
    pub new_tab: Option<bool>,
    #[option(default = TabPlacement::default())]
    pub placement: Option<TabPlacement>,
    pub tab_name: Option<String>,
    pub close_when_done: Option<bool>,
    pub nested_zmx: Option<bool>,
}

states!(
    #[derive(Default)]
    TabPlacement {
        #[default]
        Tab => "tab",
        Split => "split",
    }
);

states!(
    #[derive(Default)]
    Placement {
        #[default]
        Window => "window",
        Split => "split",
    }
);

/// A host's name: any host but the plain terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "HostName", into = "HostName")]
pub struct DefaultHost(HostName);

impl Default for DefaultHost {
    fn default() -> Self {
        Self(HostName::Zmx)
    }
}

impl TryFrom<HostName> for DefaultHost {
    type Error = String;

    fn try_from(host: HostName) -> Result<Self, String> {
        if host == HostName::Terminal {
            return Err(format!(
                "default_host must name a terminal host other than {host}"
            ));
        }
        Ok(Self(host))
    }
}

impl From<DefaultHost> for HostName {
    fn from(default: DefaultHost) -> Self {
        default.0
    }
}

impl DefaultHost {
    fn resolve<C>(self, _config: &C) -> Result<HostName> {
        Ok(self.0)
    }
}
