//! Values one machine's settings take from a command another machine of
//! the hub runs: fetched there each time, and the last one kept while that
//! machine cannot be reached.
use std::fs;

use serde_json::json;

use super::{
    directory::{join, machine, workspace},
    support::fixture::{Base, home},
};

#[test]
fn a_value_comes_from_the_machine_on_names_and_the_last_one_answers_while_it_is_away() {
    let desk = machine(false, "app");
    let token = desk.root.path().join("token");
    fs::write(&token, "sk-1").unwrap();
    let key = json!({"cmd": ["cat", token.to_str().unwrap()], "on": "desk"});
    let config = format!(
        "[machine.hub]\nenabled = true\nlisten = \"127.0.0.1:0\"\n\
         [machine.proxies.gateway]\nupstream = \"http://127.0.0.1:9\"\n\
         key = {{ cmd = [\"cat\", {:?}], on = \"desk\" }}\n",
        token.to_str().unwrap()
    );
    let hub = Base::start(home(), &config, "app", &[]);
    join(&desk, &hub, "desk");
    let worker = workspace(&hub, "topic");
    let read = || {
        let method = json!({"config_value": {"space": worker, "key": {"proxy_key": {"key": key}}}});
        let answer = hub.request(method);
        answer["data"].as_str().map(str::to_owned).ok_or(answer)
    };

    assert_eq!(read().unwrap(), "sk-1");
    // A rotated value applies at once.
    fs::write(&token, "sk-2").unwrap();
    assert_eq!(read().unwrap(), "sk-2");
    fs::write(&token, "sk-3").unwrap();
    desk.ok(&["daemon", "stop"]);
    assert_eq!(read().unwrap(), "sk-2");
}
