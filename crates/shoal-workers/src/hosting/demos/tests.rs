use super::*;
use crate::sessions::HostName;

#[test]
fn a_nested_demo_records_its_session_in_the_place_that_runs_it() {
    let worker = |place: &Origin| -> Result<Vec<OsString>> {
        let launch = serde_json::to_string(place)?;
        Ok(vec!["shoal".into(), launch.into()])
    };
    let nested = Nested {
        session: Origin::created(HostName::Zmx, "work-demo".into()),
        command: vec!["zmx".into(), "attach".into(), "work-demo".into()],
    };
    let tab = Origin::created(HostName::Herdr, "w1:p3".into());
    let argv = nested_worker(&worker, &nested)(&tab).unwrap();
    assert_eq!(argv[..4], ["zmx", "attach", "work-demo", "shoal"]);
    let recorded: Origin = serde_json::from_str(argv[4].to_str().unwrap()).unwrap();
    assert_eq!(recorded.id(), "w1:p3");
    assert_eq!(recorded.nested.unwrap().id(), "work-demo");
}
