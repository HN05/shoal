use super::*;

#[test]
fn both_adapters_preserve_single_pass_substitution() {
    let fields = [
        ("{title}", "{body}"),
        ("{body}", "$(false)\n日本語"),
        ("{empty}", ""),
    ];
    let os_fields: Vec<_> = fields
        .iter()
        .map(|(key, value)| (*key, OsStr::new(value)))
        .collect();
    for (template, expected) in [
        ("", ""),
        ("日本語 $HOME; 'quoted' \\ *", "日本語 $HOME; 'quoted' \\ *"),
        (
            "{title}: {body} {title} {unknown}",
            "{body}: $(false)\n日本語 {body} {unknown}",
        ),
        ("{title}{body}{empty}", "{body}$(false)\n日本語"),
        ("before{empty}after", "beforeafter"),
        ("{title_suffix} {Title} {}", "{title_suffix} {Title} {}"),
        ("} {title } {title", "} {title } {title"),
        ("{{title}} {", "{{body}} {"),
        ("日本語{body}後", "日本語$(false)\n日本語後"),
    ] {
        assert_eq!(render(template, &fields), expected, "{template:?}");
        assert_eq!(
            render_os(template, &os_fields),
            OsStr::new(expected),
            "{template:?}"
        );
        assert_eq!(render(template, &[]), template);
        assert_eq!(render_os(template, &[]), OsStr::new(template));
    }
}

#[cfg(unix)]
#[test]
fn os_values_preserve_path_bytes_and_placeholder_syntax() {
    use std::os::unix::ffi::OsStrExt;

    let path = OsStr::from_bytes(b"/tmp/\xff/{path}/$(false); 'quoted'");
    assert_eq!(
        render_os("--path={path}/{path}", &[("{path}", path)]).as_bytes(),
        b"--path=/tmp/\xff/{path}/$(false); 'quoted'//tmp/\xff/{path}/$(false); 'quoted'"
    );
}
