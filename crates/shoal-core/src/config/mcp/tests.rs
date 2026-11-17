use super::*;
use crate::config::{
    Config,
    repo::{ConfigLayers, parse},
    resolve::Stack,
};

/// The servers agents start under the global config, worktree file and
/// local config, by name.
fn servers(global: &str, worktree: &str, local: &str) -> Result<BTreeMap<String, Server>> {
    let global = Config::from_toml(global)?;
    let layers = ConfigLayers {
        worktree_file: parse(worktree)?,
        local_config: parse(local)?,
        ..Default::default()
    };
    let settings = Stack::new(&global, &layers)?.resolve()?;
    Ok((settings.mcp.enabled())
        .map(|(name, server)| (name.to_owned(), server.clone()))
        .collect())
}

fn server(command: &str, args: &[&str]) -> Server {
    Server {
        command: command.to_owned(),
        args: args.iter().map(|&arg| arg.to_owned()).collect(),
        env: BTreeMap::new(),
        enabled: true,
    }
}

#[test]
fn only_the_xcode_preset_brings_a_server() {
    assert!(servers("", "", "").unwrap().is_empty());
    assert!(
        servers("", "setup.presets = ['cargo']\n", "")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        servers("", "setup.presets = ['xcode']\n", "").unwrap(),
        BTreeMap::from([("xcode".to_owned(), server("xcrun", &["mcpbridge"]))])
    );
}

#[test]
fn a_table_turns_off_the_preset_server_it_names() {
    let servers = servers(
        "",
        "setup.presets = ['xcode']\n",
        "[mcp.xcode]\nenabled = false\n",
    )
    .unwrap();
    assert!(servers.is_empty());
}

#[test]
fn a_table_with_a_command_replaces_the_preset_server() {
    let servers = servers(
        "",
        "setup.presets = ['xcode']\n[mcp.xcode]\ncommand = 'xcode-mcp'\n",
        "",
    )
    .unwrap();
    assert_eq!(servers["xcode"], server("xcode-mcp", &[]));
}

#[test]
fn a_table_without_a_command_changes_the_preset_server() {
    let servers = servers(
        "",
        "setup.presets = ['xcode']\n[mcp.xcode]\nargs = ['mcpbridge', '--verbose']\nenv = { LOG = '1' }\n",
        "",
    )
    .unwrap();
    let mut expected = server("xcrun", &["mcpbridge", "--verbose"]);
    expected.env.insert("LOG".into(), "1".into());
    assert_eq!(servers["xcode"], expected);
}

#[test]
fn tables_add_servers_and_layers_merge_each_by_name() {
    let servers = servers(
        "[mcp.docs]\ncommand = 'docs-mcp'\nargs = ['--stdio']\n[mcp.search]\ncommand = 'search'\n",
        "[mcp.docs]\nenv = { TOKEN = 'a' }\n",
        "[mcp.docs]\nargs = []\n[mcp.search]\nenabled = false\n",
    )
    .unwrap();
    let mut docs = server("docs-mcp", &[]);
    docs.env.insert("TOKEN".into(), "a".into());
    assert_eq!(servers, BTreeMap::from([("docs".to_owned(), docs)]));
}

#[test]
fn a_table_that_defines_no_server_is_refused() {
    let error = servers("", "", "[mcp.docs]\nargs = ['--stdio']\n").unwrap_err();
    assert!(format!("{error:#}").contains("mcp.docs needs a command"));
    // The preset that brings the server may come from another layer.
    assert!(
        servers(
            "setup.presets = ['xcode']\n",
            "",
            "[mcp.xcode]\nenabled = false\n"
        )
        .is_ok()
    );
}

#[test]
fn a_layer_refuses_invalid_names_and_commands() {
    for text in [
        "[mcp.Docs]\ncommand = 'docs'\n",
        "[mcp.'a.b']\ncommand = 'docs'\n",
        "[mcp.docs]\ncommand = ' '\n",
        "[mcp.docs]\ncommand = 'docs'\nenv = { 'A-B' = '1' }\n",
        "[mcp.docs]\ncommand = 'docs'\ntransport = 'http'\n",
    ] {
        assert!(parse(text).is_err(), "{text}");
    }
}
