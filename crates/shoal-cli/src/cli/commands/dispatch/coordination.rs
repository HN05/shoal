use anyhow::Result;

use crate::cli::{
    args::coordination::{
        AlertCommand, CoordinationCommand, FilesCommand, QuestionAskArgs, QuestionsCommand,
    },
    commands::{alerts, files, priority, progress, questions, send, sleep, spaces},
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
            let ask = shoal_protocol::questions::Ask {
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
        CoordinationCommand::Send {
            message,
            body_file,
            attach,
            space,
            all,
            repository,
        } => {
            let recipients = match repository {
                Some(repository) => send::Recipients::Repository(repository),
                None if all => send::Recipients::All,
                None => send::Recipients::Space(space),
            };
            send::send(&ctx, recipients, message, body_file, attach).await
        }
        CoordinationCommand::Alert {
            command: AlertCommand::List(args),
        } => alerts::list(&ctx, args).await,
        CoordinationCommand::Alert {
            command: AlertCommand::Withdraw { id },
        } => alerts::withdraw(&ctx, id).await,
        CoordinationCommand::Alert {
            command: AlertCommand::Clear,
        } => alerts::clear(&ctx).await,
        CoordinationCommand::Messages { hook: _ } => alerts::delivery::deliver(&ctx).await,
        CoordinationCommand::Alerts {
            follow: _,
            keep_unread,
            resume,
        } => alerts::follow(&ctx, keep_unread, resume.unwrap_or(0)).await,
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
            add_blocker,
            reason,
            remove_blocker,
            step,
            space,
        } => {
            let blocker = progress::BlockerFlags {
                add: add_blocker,
                reason,
                remove: remove_blocker,
                step,
            };
            progress::run(&ctx, space, status, steps, blocker).await
        }
        CoordinationCommand::Sleep {
            space,
            permanent,
            undo,
            timer,
            message,
        } => {
            let sleep = match (undo, permanent) {
                (true, _) => None,
                (false, true) => Some(crate::state::Sleep::Permanent),
                (false, false) => Some(crate::state::Sleep::Turn),
            };
            let timer = sleep::timer(&timer, &message, crate::time::UnixTime::now());
            sleep::run(&ctx, space, sleep, timer).await
        }
        CoordinationCommand::Priority { priority, space } => {
            priority::run(&ctx, space, priority).await
        }
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
                spaces::undone(&ctx, space).await
            } else {
                let done = shoal_protocol::assignments::Done {
                    cleanup,
                    integrated,
                    keep_demos,
                    results: send::attachments(&attach)?,
                    summary,
                };
                // Before anything is picked or stopped.
                done.check()?;
                spaces::done(&ctx, space, done).await
            }
        }
    }
}
