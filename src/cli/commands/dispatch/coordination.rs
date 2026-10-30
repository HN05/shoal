use anyhow::Result;

use crate::cli::{
    args::coordination::{
        CoordinationCommand, FilesCommand, MessagesCommand, QuestionAskArgs, QuestionsCommand,
    },
    commands::{files, messages, questions, workspaces},
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
                in_chat,
                reason,
                choices,
                wait,
                workspace,
                resume,
            } = args;
            if in_chat {
                return questions::ask_in_chat(&ctx, workspace).await;
            }
            // Clap requires the question and recommendation without --in-chat.
            let ask = crate::daemon::questions::Ask {
                question: question.unwrap_or_default(),
                recommendation: recommend.unwrap_or_default(),
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
                    attach,
                    workspace,
                    all,
                    repository,
                }),
            ..
        } => {
            let recipients = if all {
                messages::Recipients::Repository(repository)
            } else {
                messages::Recipients::Workspace(workspace)
            };
            messages::send(&ctx, recipients, message, body_file, attach).await
        }
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
        CoordinationCommand::Files {
            command: Some(FilesCommand::Add { files, workspace }),
            ..
        } => files::add(&ctx, workspace, files).await,
        CoordinationCommand::Files {
            command:
                Some(FilesCommand::Rm {
                    files: names,
                    workspace,
                    all,
                    confirmation,
                }),
            ..
        } => {
            let scope = crate::cli::args::WorkspaceScope { workspace, all };
            files::remove(&ctx, scope, names, vec![], confirmation.yes).await
        }
        CoordinationCommand::Files {
            command: None,
            workspace,
            path,
            print,
        } => {
            let listing = match (path, print) {
                (true, _) => files::Listing::Paths,
                (false, true) => files::Listing::Print,
                (false, false) => files::Listing::Menu,
            };
            files::list(&ctx, workspace, listing).await
        }
        CoordinationCommand::Done {
            workspace,
            cleanup,
            integrated,
            undo,
            keep_demos,
            attach,
        } => {
            if undo {
                workspaces::undone(&ctx, workspace).await
            } else {
                let options = workspaces::Done {
                    cleanup,
                    integrated,
                    keep_demos,
                    results: {
                        let results = messages::attachments(&attach)?;
                        crate::daemon::workspace_files::check_results(&results)?;
                        results
                    },
                };
                workspaces::done(&ctx, workspace, options).await
            }
        }
    }
}
