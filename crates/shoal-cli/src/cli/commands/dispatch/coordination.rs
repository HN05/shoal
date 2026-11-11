use anyhow::Result;

use crate::cli::{
    args::coordination::{
        CoordinationCommand, FilesCommand, MessagesCommand, QuestionAskArgs, QuestionsCommand,
    },
    commands::{files, messages, progress, questions, workspaces},
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
                attach,
                wait,
                space,
                handed_over,
                resume,
            } = args;
            if in_chat {
                return questions::ask_in_chat(&ctx, space).await;
            }
            // Clap requires the question and recommendation without --in-chat.
            let ask = crate::client::questions::Ask {
                question: question.unwrap_or_default(),
                recommendation: recommend.unwrap_or_default(),
                reason,
                choices,
            };
            // Clap requires a space with --resume.
            match (space.or(handed_over), resume) {
                (Some(space), Some(item)) => questions::resume(&ctx, space, item, ask).await,
                (space, _) => questions::ask(&ctx, space, ask, attach, wait).await,
            }
        }
        CoordinationCommand::Questions {
            command: Some(QuestionsCommand::Withdraw { number }),
            ..
        } => questions::withdraw(&ctx, number).await,
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
            let recipients = match repository {
                Some(repository) => messages::Recipients::Repository(repository),
                None if all => messages::Recipients::All,
                None => messages::Recipients::Space(space),
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
        CoordinationCommand::Progress {
            status,
            steps,
            space,
        } => progress::run(&ctx, space, status, steps).await,
        CoordinationCommand::Done {
            space,
            cleanup,
            integrated,
            undo,
            keep_demos,
            attach,
            summary,
        } => {
            if undo {
                workspaces::undone(&ctx, space).await
            } else {
                let done = crate::client::assignments::Done {
                    cleanup,
                    integrated,
                    results: messages::attachments(&attach)?,
                    summary,
                };
                // Before anything is picked or stopped.
                done.check()?;
                workspaces::done(&ctx, space, done, keep_demos).await
            }
        }
    }
}
