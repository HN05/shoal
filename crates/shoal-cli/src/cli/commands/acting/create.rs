use crate::cli::{
    args::create::{ConvertCommand, CreateCommand},
    commands::{convert, copies, start},
    ui::Acting,
};

pub(super) fn acting(command: &CreateCommand) -> Option<Acting> {
    Some(match command {
        CreateCommand::Start(_) => start::starting(),
        CreateCommand::Create(args) => copies::copying(args.copy.checkpoint),
        CreateCommand::Convert {
            command:
                ConvertCommand::Workspace {
                    mode,
                    agent,
                    carrying,
                    ..
                },
        } => convert::to_workspace_request(mode.clone(), carrying.clone().change(agent.clone())),
        CreateCommand::Convert {
            command:
                ConvertCommand::Chat {
                    mode,
                    agent,
                    carrying,
                    ..
                },
        } => convert::to_chat_request(mode.clone(), carrying.clone().change(agent.clone())),
        CreateCommand::Convert {
            command:
                ConvertCommand::Agent {
                    tool,
                    mode,
                    carrying,
                    ..
                },
        } => convert::to_agent_request(mode.clone(), carrying.clone().change(Some(tool.clone()))),
        // A contribution forks a repository, not a space.
        CreateCommand::Contribute(_) => return None,
    })
}
