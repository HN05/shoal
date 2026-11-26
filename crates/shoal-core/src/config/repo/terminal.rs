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
    pub(crate) focus: Option<bool>,
    #[option(default = true)]
    pub(crate) close_when_done: Option<bool>,
    #[option(default = false)]
    pub(crate) nested_zmx: Option<bool>,
    #[option(section)]
    pub(crate) herdr: HerdrConfig,
    #[option(section)]
    pub(crate) rex: RexConfig,
    #[option(section)]
    pub(crate) tmux: TmuxConfig,
    #[option(section)]
    pub(crate) zellij: ZellijConfig,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Herdr)]
pub(crate) struct HerdrConfig {
    #[option(default = true)]
    pub(crate) enabled: Option<bool>,
    pub(crate) tab_name: Option<String>,
    #[option(default = true)]
    pub(crate) new_tab: Option<bool>,
    #[option(default = "{repo}".into())]
    pub(crate) workspace_name: Option<String>,
    pub(crate) close_when_done: Option<bool>,
    pub(crate) nested_zmx: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Rex)]
pub(crate) struct RexConfig {
    #[option(default = true)]
    pub(crate) enabled: Option<bool>,
    #[option(default = true)]
    pub(crate) new_window: Option<bool>,
    #[option(default = Placement::default())]
    pub(crate) placement: Option<Placement>,
    pub(crate) window_name: Option<String>,
    #[option(default = "{repo}".into())]
    pub(crate) session_name: Option<String>,
    pub(crate) close_when_done: Option<bool>,
    pub(crate) nested_zmx: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Tmux)]
pub(crate) struct TmuxConfig {
    pub(crate) enabled: Option<bool>,
    #[option(default = true)]
    pub(crate) new_window: Option<bool>,
    #[option(default = Placement::default())]
    pub(crate) placement: Option<Placement>,
    pub(crate) window_name: Option<String>,
    #[option(default = "{repo}".into())]
    pub(crate) session_name: Option<String>,
    pub(crate) close_when_done: Option<bool>,
    pub(crate) nested_zmx: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Zellij)]
pub(crate) struct ZellijConfig {
    pub(crate) enabled: Option<bool>,
    #[option(default = true)]
    pub(crate) new_tab: Option<bool>,
    #[option(default = TabPlacement::default())]
    pub(crate) placement: Option<TabPlacement>,
    pub(crate) tab_name: Option<String>,
    pub(crate) close_when_done: Option<bool>,
    pub(crate) nested_zmx: Option<bool>,
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
