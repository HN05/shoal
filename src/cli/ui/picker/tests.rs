use super::*;

#[test]
fn enclosed_arguments_pick_delimiters_they_do_not_close() {
    assert_eq!(enclosed("reload", "ls").unwrap(), "reload(ls)");
    assert_eq!(
        enclosed("reload", "echo (a) [b]").unwrap(),
        "reload<echo (a) [b]>"
    );
    assert!(enclosed("reload", ")]>~!@#^").is_err());
}

#[test]
fn in_place_keys_show_the_outcome_above_the_hints_then_reload() {
    let key = InPlaceKey {
        key: "ctrl-a",
        label: "approve",
        command: "'/bin/shoal (dev)' decide".into(),
    };
    assert_eq!(
        key.binding("enter: open   ctrl-a: approve", "'/bin/shoal (dev)' rows")
            .unwrap(),
        "ctrl-a:transform-header['/bin/shoal (dev)' decide {1} 2>&1; \
         printf '%s\\n' 'enter: open   ctrl-a: approve']+reload:'/bin/shoal (dev)' rows"
    );
}
