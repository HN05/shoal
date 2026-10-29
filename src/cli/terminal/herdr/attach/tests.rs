use super::*;
use crate::sessions::HostName;

fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

#[test]
fn the_socket_decides_the_server_before_the_session_name() {
    let recorded = vars(&[
        ("HERDR_SOCKET_PATH", "/h/work/herdr.sock"),
        ("HERDR_SESSION", "work"),
    ]);
    let here = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(var, _)| *var == name)
                .map(|(_, value)| (*value).to_owned())
        }
    };
    assert!(same_server(
        &recorded,
        here(&[("HERDR_SOCKET_PATH", "/h/work/herdr.sock")])
    ));
    assert!(!same_server(
        &recorded,
        here(&[
            ("HERDR_SOCKET_PATH", "/h/other/herdr.sock"),
            ("HERDR_SESSION", "work"),
        ])
    ));
    assert!(!same_server(&recorded, here(&[("HERDR_SESSION", "other")])));
    // Without a variable to compare, only a pane that recorded neither
    // shares the current server.
    let session_only = vars(&[("HERDR_SESSION", "work")]);
    assert!(!same_server(
        &session_only,
        here(&[("HERDR_SOCKET_PATH", "/h/work/herdr.sock")])
    ));
    assert!(same_server(&vars(&[]), here(&[("HERDR_SESSION", "other")])));
}

#[test]
fn tab_info_reads_the_label() {
    let info: TabInfo = serde_json::from_str(
        r#"{"id":"cli:tab:get","result":{"tab":{"agent_status":"idle","label":"shoal#1328","tab_id":"w1:t2"},"type":"tab_info"}}"#,
    )
    .unwrap();
    assert_eq!(info.result.tab.label.as_deref(), Some("shoal#1328"));
}

#[test]
fn outside_herdr_only_a_recorded_session_is_attached() {
    let outside = |_: &str| None;
    let socket_only = Origin {
        data: vars(&[("HERDR_SOCKET_PATH", "/h/work/herdr.sock")]),
        ..Origin::created(HostName::Herdr, "w1:p1".to_owned())
    };
    let error = client_session(&socket_only, outside).unwrap_err();
    assert!(
        error.to_string().contains("recorded no session"),
        "{error:#}"
    );
    let recorded = Origin {
        data: vars(&[("HERDR_SESSION", "work")]),
        ..socket_only.clone()
    };
    assert_eq!(client_session(&recorded, outside).unwrap(), Some("work"));
    // A pane of the same server already shows it.
    let inside = |name: &str| match name {
        "HERDR_ENV" => Some("1".to_owned()),
        "HERDR_SESSION" => Some("work".to_owned()),
        _ => None,
    };
    assert_eq!(client_session(&recorded, inside).unwrap(), None);
}
