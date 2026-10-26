use super::PrGates;

#[tokio::test]
async fn gates_exclude_only_the_same_workspace() {
    let gates = PrGates::default();
    let held = gates.lock(["a", "b"]).await;
    assert!(gates.try_lock(["c"]).is_some());
    assert!(gates.try_lock(["b"]).is_none());
    assert!(gates.try_lock(["c", "a"]).is_none());
    drop(held);
    assert!(gates.try_lock(["a", "b", "c"]).is_some());
}

#[tokio::test]
async fn unused_gates_are_forgotten() {
    let gates = PrGates::default();
    let held = gates.lock(["a"]).await;
    drop(gates.lock(["b", "c"]).await);
    assert!(gates.try_lock(["a"]).is_none());
    assert_eq!(gates.0.lock().unwrap().len(), 1);
    drop(held);
}
