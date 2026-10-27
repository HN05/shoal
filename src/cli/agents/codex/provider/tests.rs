use super::*;

const URL: &str = "http://127.0.0.1:4000";

/// `codex` with Shoal's placeholder and then `user`'s arguments, routed by
/// the Codex `config`.
fn routed(config: &str, user: &[&str]) -> Result<Vec<String>> {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("config.toml");
    std::fs::write(&path, config).unwrap();
    let mut args: Vec<OsString> = ["codex", PROVIDER].into_iter().map(Into::into).collect();
    args.extend(user.iter().map(Into::into));
    route(&mut args, URL, &path)?;
    Ok(args
        .into_iter()
        .map(|arg| arg.into_string().unwrap())
        .collect())
}

/// The provider whose base URL `args` move to the proxy.
fn moved(args: &[String]) -> Option<&str> {
    args.iter().find_map(|arg| {
        arg.strip_prefix("model_providers.")?
            .strip_suffix(&format!(".base_url=\"{URL}\""))
    })
}

#[test]
fn a_forwarded_override_or_profile_selects_the_provider_that_moves() {
    let config = "model_provider = 'cliproxy'\n\
                  [profiles.work]\nmodel_provider = 'work-proxy'\n\
                  [profiles.plain]\nmodel = 'gpt-5'\n";
    for (user, provider) in [
        (&[][..], "cliproxy"),
        (&["-c", "model_provider=other"][..], "other"),
        (&["--config", "model_provider=\"other\""][..], "other"),
        (&["--config=model_provider=other"][..], "other"),
        (&["-cmodel_provider=other"][..], "other"),
        // A later override wins, as in Codex.
        (
            &["-c", "model_provider=first", "-c", "model_provider=second"][..],
            "second",
        ),
        (&["--profile", "work"][..], "work-proxy"),
        (&["-p", "work"][..], "work-proxy"),
        (&["--profile=work"][..], "work-proxy"),
        // A profile's provider wins over a top-level override.
        (
            &["-p", "work", "-c", "model_provider=other"][..],
            "work-proxy",
        ),
        (
            &["-c", "profiles.work.model_provider=moved", "-p", "work"][..],
            "moved",
        ),
        // A profile without a provider keeps the top-level one.
        (&["-p", "plain"][..], "cliproxy"),
        (&["-c", "profile=work"][..], "work-proxy"),
        // A prompt after `--` is no option.
        (&["--", "-c", "model_provider=other"][..], "cliproxy"),
    ] {
        let args = routed(config, user).unwrap();
        assert_eq!(moved(&args), Some(provider), "{user:?}: {args:?}");
        assert!(args.iter().all(|arg| arg != PROVIDER), "{args:?}");
    }
    let config = "profile = 'work'\n[profiles.work]\nmodel_provider = 'work-proxy'\n";
    assert_eq!(moved(&routed(config, &[]).unwrap()), Some("work-proxy"));
    let quoted = "[profiles.\"my work\"]\nmodel_provider = 'quoted'\n";
    let args = routed(quoted, &["-p", "my work"]).unwrap();
    assert_eq!(moved(&args), Some("quoted"));
}

/// The value `args` leave dotted `key` with, as Codex applies their
/// overrides in order.
fn effective(args: &[String], key: &str) -> Option<toml::Value> {
    let args: Vec<OsString> = args.iter().map(Into::into).collect();
    let key: Vec<String> = key.split('.').map(str::to_owned).collect();
    Flags::of(&args)
        .overrides
        .into_iter()
        .rev()
        .find(|config| config.key == key)
        .map(|config| config.value)
}

#[test]
fn the_built_in_provider_gives_way_to_shoals_wherever_the_launch_names_it() {
    let shoal = Some(toml::Value::String("shoal-proxy".into()));
    // Without a provider, or with the user's own choice of the built-in one,
    // which Shoal's follows.
    for (config, user) in [
        ("", &[][..]),
        ("model_provider = 'openai'\n", &[][..]),
        (
            "model_provider = 'cliproxy'\n",
            &["-c", "model_provider=openai", "exec"][..],
        ),
    ] {
        let args = routed(config, user).unwrap();
        assert_eq!(effective(&args, "model_provider"), shoal, "{args:?}");
        let provider = effective(&args, "model_providers.shoal-proxy").unwrap();
        assert_eq!(provider["requires_openai_auth"].as_bool(), Some(true));
        assert_eq!(provider["base_url"].as_str(), Some(URL));
        // The launch's own arguments stay, in order, around Shoal's four.
        let at = args
            .iter()
            .position(|arg| arg.contains("shoal-proxy"))
            .unwrap()
            - 1;
        let mut own = args.clone();
        own.drain(at..at + 4);
        assert_eq!(own[1..], *user, "{args:?}");
    }
    // A profile on it is pointed at Shoal's provider where it names it.
    let config = "[profiles.\"my work\"]\nmodel_provider = 'openai'\n";
    let args = routed(config, &["-p", "my work"]).unwrap();
    assert!(
        args.contains(&"profiles.\"my work\".model_provider=\"shoal-proxy\"".to_owned()),
        "{args:?}"
    );
    assert_eq!(args[args.len() - 2..], ["-p", "my work"]);
}

#[test]
fn shoals_overrides_win_over_the_provider_settings_a_launch_forwards() {
    let config = "model_provider = 'cliproxy'\n";
    for forwarded in [
        "model_providers.cliproxy.base_url=\"http://elsewhere\"",
        "model_providers.cliproxy={base_url=\"http://elsewhere\",env_key=\"KEY\"}",
        "model_providers.cliproxy.supports_websockets=true",
    ] {
        let args = routed(config, &["-c", forwarded, "exec", "--", "-c", "prompt"]).unwrap();
        let ours = args
            .iter()
            .position(|arg| arg == &format!("model_providers.cliproxy.base_url=\"{URL}\""))
            .unwrap();
        let theirs = args.iter().position(|arg| arg == forwarded).unwrap();
        assert!(theirs < ours, "{args:?}");
        // Before the launch's prompt, which `--` keeps as it is.
        let end = args.iter().position(|arg| arg == "--").unwrap();
        assert!(ours < end, "{args:?}");
        assert_eq!(args[end..], ["--", "-c", "prompt"]);
    }
    // On the built-in provider, a forwarded table for Shoal's comes first.
    let forwarded = "model_providers.shoal-proxy={base_url=\"http://elsewhere\"}";
    let args = routed("", &["-c", forwarded]).unwrap();
    let provider = effective(&args, "model_providers.shoal-proxy").unwrap();
    assert_eq!(provider["base_url"].as_str(), Some(URL), "{args:?}");
}

#[test]
fn a_built_in_provider_the_proxy_cannot_take_is_refused() {
    for (config, user) in [
        ("model_provider = 'oss'\n", &[][..]),
        ("", &["-c", "model_provider=ollama"][..]),
        ("", &["--oss"][..]),
    ] {
        let error = format!("{:#}", routed(config, user).unwrap_err());
        assert!(
            error.contains("cannot go through Shoal's proxy"),
            "{user:?}: {error}"
        );
    }
}
