use shoal_spaces::workspace_usage::*;
use tokio::time::Instant;

#[tokio::test]
async fn a_chats_processes_count_in_the_sampled_work_memory() {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    let chat = crate::daemon::test_support::chat(&manager, "notes", "chat", None, None).await;
    let _child = crate::daemon::test_support::spawn_marked_child(
        crate::daemon::test_support::marked_child().env(crate::env::SPACE, &chat.id),
    );
    let mut usages = WorkspaceUsages::default();
    let sampled =
        shoal_spaces::workspace_usage::sample(&manager.store, Vec::new(), Default::default()).await;
    usages.record(sampled, Ok(Vec::new()));
    let now = Instant::now();
    let chat_memory = usages.get(&chat.id, now).unwrap().memory_bytes;
    assert!(chat_memory > 0);
    assert!(usages.memory(now).unwrap().all >= chat_memory);
}
