use super::*;

#[test]
fn a_forge_number_serializes_as_the_number_it_holds() {
    let run = RunNumber::new(42);
    assert_eq!(serde_json::to_value(run).unwrap(), serde_json::json!(42));
    assert_eq!(
        serde_json::from_value::<RunNumber>(serde_json::json!(42)).unwrap(),
        run
    );
}
