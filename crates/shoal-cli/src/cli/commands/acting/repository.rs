use crate::cli::{
    args::repository::{ForgeCommand, RepoCommand, RepositoryCommand},
    ui::Acting,
};

pub(super) fn acting(command: &RepositoryCommand) -> Option<Acting> {
    match command {
        RepositoryCommand::Repo {
            command:
                None
                | Some(
                    RepoCommand::Labels(_)
                    | RepoCommand::Sync { .. }
                    | RepoCommand::Config { .. }
                    | RepoCommand::Add { .. }
                    | RepoCommand::Rename { .. }
                    | RepoCommand::Share { .. }
                    | RepoCommand::Unshare { .. }
                    | RepoCommand::Setup { .. }
                    | RepoCommand::Rm { .. }
                    | RepoCommand::Move(_)
                    | RepoCommand::Ls,
                ),
        }
        | RepositoryCommand::Forge {
            command:
                ForgeCommand::Capabilities(_)
                | ForgeCommand::Reactions(_)
                | ForgeCommand::Api(_)
                | ForgeCommand::Download(_),
        } => None,
    }
}
