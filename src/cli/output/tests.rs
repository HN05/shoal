use super::*;

#[test]
fn color_requires_a_human_terminal_without_an_opt_out() {
    for (terminal, json, no_color, term, enabled) in [
        (true, false, None, None, true),
        (false, false, None, None, false),
        (true, true, None, None, false),
        (true, false, Some("1"), None, false),
        (true, false, Some("0"), None, false),
        (true, false, Some(""), None, true),
        (true, false, None, Some("dumb"), false),
        (true, false, None, Some("xterm-256color"), true),
    ] {
        let palette = Palette::for_environment(
            terminal,
            json,
            no_color.map(OsStr::new),
            term.map(OsStr::new),
        );
        assert_eq!(palette.enabled, enabled);
        let output = palette.paint(Style::Error, "error:");
        assert_eq!(output.contains('\x1b'), enabled);
        if !enabled {
            assert_eq!(output, "error:");
        }
    }
}

#[test]
fn styles_reset_and_state_labels_keep_their_wire_spelling() {
    let palette = Palette { enabled: true };
    assert_eq!(
        palette.workspace_state(WorkspaceState::Ready),
        "\x1b[32mready\x1b[0m"
    );
    assert_eq!(
        palette.workspace_state(WorkspaceState::Failed),
        "\x1b[1;31mfailed\x1b[0m"
    );
    assert_eq!(
        palette.execution_state(ExecutionState::Unknown),
        "\x1b[33munknown\x1b[0m"
    );
    let plain = Palette { enabled: false };
    assert_eq!(
        plain.workspace_state(WorkspaceState::Preparing),
        "preparing"
    );
    assert_eq!(
        serde_json::to_string(&WorkspaceState::Ready).unwrap(),
        "\"ready\""
    );
}

#[test]
fn emoji_require_a_utf8_terminal_with_glyphs() {
    for (terminal, term, locale, shown) in [
        (true, Some("xterm-256color"), Some("en_US.UTF-8"), true),
        (true, None, Some("C.utf8"), true),
        (false, Some("xterm-256color"), Some("en_US.UTF-8"), false),
        (true, Some("xterm-256color"), Some("C"), false),
        (true, Some("xterm-256color"), None, false),
        (true, Some("linux"), Some("en_US.UTF-8"), false),
        (true, Some("dumb"), Some("en_US.UTF-8"), false),
    ] {
        assert_eq!(
            shows_emoji(terminal, term.map(OsStr::new), locale.map(OsStr::new)),
            shown,
            "{terminal} {term:?} {locale:?}"
        );
    }
}
