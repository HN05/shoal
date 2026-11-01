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
                space,
                handed_over,
                resume,
            } = args;
            if in_chat {
                return questions::ask_in_chat(&ctx, space).await;
            }
            // Clap requires the question and recommendation without --in-chat.
            let ask = crate::daemon::questions::Ask {
                question: question.unwrap_or_default(),
                recommendation: recommend.unwrap_or_default(),
                reason,
                choices,
            };
            // Clap requires a space with --resume.
            match (space.or(handed_over), resume) {
                (Some(space), Some(id)) => questions::resume(&ctx, space, id, ask).await,
                (space, _) => questions::ask(&ctx, space, ask, wait).await,
            }
        }
        CoordinationCommand::Questions {
            command: Some(QuestionsCommand::Withdraw { id, space }),
            ..
        } => questions::withdraw(&ctx, space, id).await,
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
                    space,
                    all,
                    repository,
                }),
            ..
        } => {
            let recipients = if all {
                messages::Recipients::Repository(repository)
            } else {
                messages::Recipients::Space(space)
            };
            messages::send(&ctx, recipients, message, body_file, attach).await
        }
        CoordinationCommand::Messages {
            command: None,
            space,
            hook,
        } => {
            if hook {
                messages::hook(&ctx).await
            } else {
                messages::show(&ctx, space).await
            }
        }
        CoordinationCommand::Files {
            command: Some(FilesCommand::Add { files, space }),
            ..
        } => files::add(&ctx, space, files).await,
        CoordinationCommand::Files {
            command:
                Some(FilesCommand::Rm {
                    files: names,
                    space,
                    all,
                    confirmation,
                }),
            ..
        } => {
            let scope = crate::cli::args::WorkspaceScope {
                workspace: space,
                all,
            };
            files::remove(&ctx, scope, names, vec![], confirmation.yes).await
        }
        CoordinationCommand::Files {
            command: None,
            space,
            path,
            print,
        } => {
            let listing = match (path, print) {
                (true, _) => files::Listing::Paths,
                (false, true) => files::Listing::Print,
                (false, false) => files::Listing::Menu,
            };
            files::list(&ctx, space, listing).await
        }
        CoordinationCommand::Done {
            space,
            cleanup,
            integrated,
            undo,
            keep_demos,
            attach,
        } => {
            if undo {
                workspaces::undone(&ctx, space).await
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
                workspaces::done(&ctx, space, options).await
            }
        }
    }
}
