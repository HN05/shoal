use serde_json::json;

use super::*;

/// A machine `id`, named as its ID, online unless `offline`.
fn member(id: &str, offline: bool) -> serde_json::Value {
    json!({
        "id": id, "kind": "machine", "name": id, "joined_at": 0,
        "last_seen": offline.then_some(0), "connected": !offline, "hub": false,
    })
}

/// The hub's view from the machine `here`, with `desk` and `laptop`, of
/// which `offline` are offline.
fn view(offline: &[&str]) -> HubView {
    let machines: Vec<_> = (["here", "desk", "laptop"].into_iter())
        .map(|id| member(id, offline.contains(&id)))
        .collect();
    serde_json::from_value(json!({"machine": "here", "machines": machines, "spaces": []})).unwrap()
}

/// The registration `id` of `machine`, named `name`, of `origin`.
fn other(machine: &str, id: &str, name: &str, origin: Option<&str>) -> RepositoryElsewhere {
    serde_json::from_value(json!({
        "machine": machine, "machine_id": machine, "id": id, "name": name, "origin": origin,
    }))
    .unwrap()
}

fn local(name: &str) -> Repository {
    serde_json::from_value(json!({
        "id": format!("{name}-here"), "path": format!("/{name}"), "source": format!("/{name}"),
        "last_used": 0, "name": name, "workspaces_dir": format!("/shoal/{name}"),
    }))
    .unwrap()
}

/// Where each target acts, as "here" or the machine and the ID there.
fn places(resolved: Result<Resolved>) -> Vec<String> {
    match resolved.unwrap() {
        Resolved::Targets(targets) => (targets.into_iter())
            .map(|target| match target {
                Target::Here(_) => "here".to_owned(),
                Target::Elsewhere(other) => format!("{}:{}", other.machine, other.registered.id),
            })
            .collect(),
        Resolved::Choose(_) => panic!("expected targets"),
    }
}

/// The registrations: `web` on desk and laptop under the same origin,
/// `api` on desk alone.
fn elsewhere() -> Vec<RepositoryElsewhere> {
    vec![
        other("desk", "d-web", "web", Some("forge/web")),
        other("desk", "d-api", "api", Some("forge/api")),
        other("laptop", "l-site", "site", Some("forge/web")),
    ]
}

fn registered<'a>(
    here: Option<&'a Repository>,
    elsewhere: &'a [RepositoryElsewhere],
    view: &'a HubView,
) -> Registered<'a> {
    Registered {
        here,
        here_origin: here.map(|_| "forge/web".to_owned()),
        elsewhere,
        view: Some(view),
    }
}

#[test]
fn one_registered_here_is_removed_here_and_another_s_on_its_one_machine() {
    let (web, elsewhere, view) = (local("web"), elsewhere(), view(&[]));
    let found = registered(Some(&web), &elsewhere, &view);
    assert_eq!(places(resolve("web", &found, &Machines::Found)), ["here"]);
    let found = registered(None, &elsewhere, &view);
    assert_eq!(
        places(resolve("api", &found, &Machines::Found)),
        ["desk:d-api"]
    );
    assert_eq!(
        places(resolve("nothing", &found, &Machines::Found)),
        Vec::<String>::new()
    );
}

#[test]
fn one_registered_on_several_other_machines_is_chosen_among_them() {
    let elsewhere = vec![
        other("desk", "d-web", "web", None),
        other("laptop", "l-web", "web", None),
    ];
    let view = view(&[]);
    let found = registered(None, &elsewhere, &view);
    let Resolved::Choose(candidates) = resolve("web", &found, &Machines::Found).unwrap() else {
        panic!("expected a choice");
    };
    let machines: Vec<&str> = candidates
        .iter()
        .map(|other| other.machine.as_str())
        .collect();
    assert_eq!(machines, ["desk", "laptop"]);
}

#[test]
fn every_machine_takes_its_registration_of_the_name_or_else_of_the_origin() {
    let (web, elsewhere, view) = (local("web"), elsewhere(), view(&[]));
    let found = registered(Some(&web), &elsewhere, &view);
    assert_eq!(
        places(resolve("web", &found, &Machines::All)),
        ["here", "desk:d-web", "laptop:l-site"]
    );
    let named = Machines::Named(vec!["laptop".into(), "here".into(), "laptop".into()]);
    assert_eq!(
        places(resolve("web", &found, &named)),
        ["laptop:l-site", "here"]
    );
}

#[test]
fn a_named_machine_without_it_is_refused() {
    let (elsewhere, view) = (elsewhere(), view(&[]));
    let found = registered(None, &elsewhere, &view);
    let named = Machines::Named(vec!["laptop".into()]);
    let error = resolve("api", &found, &named).unwrap_err().to_string();
    assert!(
        error.contains("machine laptop registers no repository api"),
        "{error}"
    );
}

#[test]
fn an_offline_machine_it_would_act_on_refuses_the_removal_naming_it() {
    let (web, elsewhere, view) = (local("web"), elsewhere(), view(&["laptop"]));
    let found = registered(Some(&web), &elsewhere, &view);
    let error = resolve("web", &found, &Machines::All)
        .unwrap_err()
        .to_string();
    assert!(error.contains("machine laptop is offline"), "{error}");
    assert!(!error.contains("desk"), "{error}");
    // One that registers nothing of it is no reason to refuse.
    let found = registered(None, &elsewhere, &view);
    assert_eq!(
        places(resolve("api", &found, &Machines::All)),
        ["desk:d-api"]
    );
}
