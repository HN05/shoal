use anyhow::Result;

use crate::cli::{
    args::coordination::{CoordinationCommand, MessagesCommand, QuestionAskArgs, QuestionsCommand},
    commands::{messages, questions, workspaces},
    context::Context,
};

pub(in crate::cli::commands) async fn run(
    ctx: Context,
    command: CoordinationCommand,
) -> Result<i32> {
    match command {
        CoordinationCommand::Questions {
            command: Some(QuestionsCommand::Ask(args)),
            ..
        } => {
            let QuestionAskArgs {
                question,
                recommend,
                reason,
                choices,
                wait,
                workspace,
                resume,
            } = args;
            let ask = crate::daemon::questions::Ask {
                question,
                recommendation: recommend,
                reason,
                choices,
            };
            // Clap requires --workspace with --resume.
            match (workspace, resume) {
                (Some(workspace), Some(id)) => questions::resume(&ctx, workspace, id, ask).await,
                (workspace, _) => questions::ask(&ctx, workspace, ask, wait).await,
            }
        }
        CoordinationCommand::Questions {
            command: Some(QuestionsCommand::Withdraw { id, workspace }),
            ..
        } => questions::withdraw(&ctx, workspace, id).await,
        CoordinationCommand::Questions {
            command: None,
            args,
        } => questions::run(&ctx, args).await,
        CoordinationCommand::Messages {
            command:
                Some(MessagesCommand::Send {
                    message,
                    body_file,
                    workspace,
                }),
            ..
        } => messages::send(&ctx, workspace, message, body_file).await,
        CoordinationCommand::Messages {
            command: None,
            workspace,
            hook,
        } => {
            if hook {
                messages::hook(&ctx).await
            } else {
                messages::show(&ctx, workspace).await
            }
        }
        CoordinationCommand::Done {
            workspace,
            cleanup,
            integrated,
            undo,
            keep_demos,
        } => {
            if undo {
                workspaces::undone(&ctx, workspace).await
            } else {
                let options = workspaces::Done {
                    cleanup,
                    integrated,
                    keep_demos,
                };
                workspaces::done(&ctx, workspace, options).await
            }
        }
    }
}
