//! Service commands: the request each sends for the space it acts on.
use crate::cli::{
    args::{
        hub::HubCommand,
        service::{ConfigCommand as Config, DaemonCommand, ServiceCommand as Service},
    },
    commands::configuration,
    ui::Acting,
};

/// What a service command sends for the workspace it names; only showing
/// a workspace's configuration picks one.
pub(super) fn acting(command: &Service) -> Option<Acting> {
    match command {
        Service::Config {
            command: Config::Show { .. },
        } => Some(configuration::reading()),
        Service::Config {
            command:
                Config::Set { .. }
                | Config::Unset { .. }
                | Config::Reference
                | Config::Install { .. }
                | Config::Reset
                | Config::Reload
                | Config::Setup(_),
        } => None,
        Service::Update { .. }
        | Service::Daemon {
            command:
                DaemonCommand::Install { .. }
                | DaemonCommand::Start
                | DaemonCommand::Stop(_)
                | DaemonCommand::Restart(_)
                | DaemonCommand::Run { .. }
                | DaemonCommand::Connect,
        }
        | Service::Hub {
            command:
                HubCommand::Pair
                | HubCommand::Join(_)
                | HubCommand::Leave
                | HubCommand::Ls
                | HubCommand::Update
                | HubCommand::Revoke { .. },
        } => None,
    }
}
