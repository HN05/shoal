use anyhow::Result;

use crate::cli::{
    args::{
        coordination::{AlertCommand, CoordinationCommand, FilesCommand},
        threads::ThreadCommand,
    },
    commands::{alerts, files, priority, progress, send, sleep, spaces, threads},
    context::Context,
};

pub(in crate::cli::commands) async fn run(
    ctx: Context,
    command: CoordinationCommand,
) -> Result<i32> {
    match command {
        CoordinationCommand::Send {
            args,
            body_file,
            attach,
            space,
            all,
            repository,
            post,
        } => match send::sending(args, body_file.is_some(), &post)? {
            send::Sending::Alert(message) => {
                let recipients = match repository {
                    Some(repository) => send::Recipients::Repository(repository),
                    None if all => send::Recipients::All,
                    None => send::Recipients::Space(space),
                };
                send::send(&ctx, recipients, message, body_file, attach).await
            }
            send::Sending::Post { thread, text } => {
                let posting = threads::Posting {
                    thread,
                    text,
                    body_file,
                    attach,
                    options: post,
                };
                threads::post(&ctx, posting).await
            }
        },
        CoordinationCommand::Thread { command } => thread(&ctx, command).await,
        CoordinationCommand::Alert {
            command: AlertCommand::List(args),
        } => alerts::list(&ctx, args).await,
        CoordinationCommand::Alert {
            command: AlertCommand::Withdraw { id, space },
        } => alerts::withdraw(&ctx, id, space).await,
        CoordinationCommand::Alert {
            command: AlertCommand::Clear,
        } => alerts::clear(&ctx).await,
        CoordinationCommand::Messages { hook: _ } => alerts::delivery::deliver(&ctx).await,
        CoordinationCommand::Alerts {
            keep_unread,
            resume,
            ..
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

/// Run a `shoal thread` command.
async fn thread(ctx: &Context, command: ThreadCommand) -> Result<i32> {
    match command {
        ThreadCommand::Open { name, kind, space } => threads::open(ctx, name, kind, space).await,
        ThreadCommand::Forward {
            thread,
            space,
            note,
        } => threads::forward(ctx, thread, space, note).await,
        ThreadCommand::Resolve {
            thread,
            outcome,
            choose,
            of,
            reason,
            undo,
        } => {
            let resolving = threads::resolving(outcome, choose, of, reason, undo)?;
            threads::resolve(ctx, thread, resolving).await
        }
        ThreadCommand::List {
            space,
            state,
            print,
        } => threads::list(ctx, space, state, print).await,
    }
}
