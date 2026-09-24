//! Review daemon-owned access requests and render allocation decisions.
use anyhow::Result;
use serde_json::json;

use super::{Attempt, EXIT_BUSY};
use crate::{
    access::{AccessRequest, Status},
    cli::AccessCommand,
    client::request,
    context::Context,
    output::Style,
    protocol::Method,
};

pub(super) async fn run(ctx: &Context, command: Option<AccessCommand>) -> Result<i32> {
    match command {
        Some(command @ (AccessCommand::Approve { .. } | AccessCommand::Deny { .. })) => {
            let approve = matches!(command, AccessCommand::Approve { .. });
            let (AccessCommand::Approve { id } | AccessCommand::Deny { id }) = command else {
                unreachable!()
            };
            let request =
                request::<Box<AccessRequest>>(&ctx.paths, Method::DecideAccess { id, approve })
                    .await?;
            ctx.emit(&describe(&request), &request)?;
            Ok(0)
        }
        Some(AccessCommand::List { workspace }) => list(ctx, workspace).await,
        None => list(ctx, None).await,
    }
}

async fn list(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let requests =
        request::<Vec<AccessRequest>>(&ctx.paths, Method::ListAccess { workspace }).await?;
    ctx.show(&requests, |requests| {
        for request in requests {
            println!("{}", describe(request));
            println!("  {}", request.specification);
        }
        if requests.is_empty() {
            println!("No access requests");
        }
    })?;
    Ok(0)
}

fn describe(request: &AccessRequest) -> String {
    format!(
        "{} [{}] {} / {} (workspace {}, {}): {}",
        request.id,
        request.status,
        request.target,
        request.name,
        request.workspace,
        request.lifetime,
        request.reason
    )
}

pub(super) fn attempt<T>(
    last: &mut Option<Box<AccessRequest>>,
    request: Box<AccessRequest>,
) -> Attempt<Result<T, Box<AccessRequest>>> {
    if request.status == Status::Denied {
        Attempt::Ready(Err(request))
    } else {
        let message = describe(&request);
        *last = Some(request);
        Attempt::Busy(message)
    }
}

pub(super) fn declined(ctx: &Context, request: &AccessRequest) -> Result<i32> {
    let code = if request.status == Status::Denied {
        "approval_denied"
    } else {
        "approval_pending"
    };
    ctx.emit_styled(
        Style::Warning,
        &describe(request),
        json!({"acquired": false, "code": code, "request": request}),
    )?;
    Ok(EXIT_BUSY)
}
