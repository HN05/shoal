use super::Gates;

#[tokio::test]
async fn pr_gates_exclude_only_the_same_workspace() {
    let gates = Gates::default();
    let held = gates.lock_prs(["a", "b"]).await;
    assert!(gates.try_lock_prs(["c"]).is_some());
    assert!(gates.try_lock_prs(["b"]).is_none());
    assert!(gates.try_lock_prs(["c", "a"]).is_none());
    drop(held);
    assert!(gates.try_lock_prs(["a", "b", "c"]).is_some());
}

#[tokio::test]
async fn unused_gates_are_forgotten() {
    let gates = Gates::default();
    let held = gates.lock_prs(["a"]).await;
    drop(gates.lock_prs(["b", "c"]).await);
    assert!(gates.try_lock_prs(["a"]).is_none());
    assert_eq!(gates.pr_gates_in_use(), 1);
    drop(held);
}
