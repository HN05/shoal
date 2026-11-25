use shoal_protocol::space_names::SpaceSelector;

use super::*;
use crate::executions::ExecutionKind;
use crate::stand_in::Fixture;

/// What a wrapper in this process asks to run `agent`, or a plain command,
/// in the space `space`.
fn context(space: &shoal_ids::space_ids::SpaceId, agent: Option<&str>) -> ExecutionContext {
    ExecutionContext {
        space: SpaceSelector::id(space),
        wrapper: crate::process::identity::capture(std::process::id())
            .unwrap()
            .unwrap(),
        kind: ExecutionKind::Command,
        agent: agent.map(shoal_protocol::executions::TrackedAgent::named),
        recover: true,
        reattach: true,
        queue_entry: None,
        demo: None,
        origin: None,
        merge: None,
        caller: None,
    }
}

/// The error a refused registration answers with.
fn refusal(registered: Result<Registered, Box<Body>>) -> String {
    match registered.map(drop).unwrap_err().as_ref() {
        Body::Error { message, .. } => message.clone(),
        body => panic!("{body:?}"),
    }
}

#[tokio::test]
async fn a_chat_runs_its_one_agent_and_no_commands() {
    let fixture = Fixture::new().await;
    let repo = fixture.repository().await;
    let chat = fixture.chat_in("talk", "chat", Some(&repo)).await;
    let (agents, daemon) = (&fixture.agents, &*fixture.daemon);
    // Stopped, so the wrapper below registers its agent.
    agents.end_executions(daemon, &chat).await;

    let command = refusal(register(agents, daemon, context(&chat, None)).await);
    assert!(command.contains("runs only its agent"), "{command}");
    let _agent = register(agents, daemon, context(&chat, Some("claude")))
        .await
        .map_err(|refusal| format!("{refusal:?}"))
        .unwrap();
    let second = refusal(register(agents, daemon, context(&chat, Some("codex"))).await);
    assert!(second.contains("already runs its agent"), "{second}");
}
