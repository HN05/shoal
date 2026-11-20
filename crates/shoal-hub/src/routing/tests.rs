use serde_json::json;

use super::launch_machine;

#[test]
fn a_device_s_launch_goes_to_the_machine_it_names_else_the_default() {
    let launch = |machine: Option<&str>| {
        let mut launch = json!({"chat": true});
        if let Some(machine) = machine {
            launch["machine"] = json!(machine);
        }
        json!({"launch": {"launch": launch}})
    };
    let mut named = launch(Some("laptop"));
    let target = launch_machine(&mut named, Some("desk".into())).unwrap();
    assert_eq!(target.as_deref(), Some("laptop"));
    // The launch names no machine once routed, so it starts where it goes.
    assert!(
        named["launch"]["launch"].get("machine").is_none(),
        "{named}"
    );
    let mut unnamed = launch(None);
    let target = launch_machine(&mut unnamed, Some("desk".into())).unwrap();
    assert_eq!(target.as_deref(), Some("desk"));
    assert_eq!(launch_machine(&mut launch(None), None).unwrap(), None);
    let mut other = json!({"rename_space": {"space": {"number": 1}, "name": "x"}});
    assert_eq!(
        launch_machine(&mut other, Some("desk".into())).unwrap(),
        None
    );
}

#[test]
fn a_device_s_read_of_a_launch_s_modes_goes_where_the_launch_would() {
    let mut modes = json!({"launch_modes": {"launch": {}}});
    let target = launch_machine(&mut modes, Some("desk".into())).unwrap();
    assert_eq!(target.as_deref(), Some("desk"));
}
