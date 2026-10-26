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

#[test]
fn the_built_in_provider_gives_way_to_shoals_wherever_the_launch_names_it() {
    let shoal = "model_provider=\"shoal-proxy\"";
    // Without a provider, or with the user's own choice of the built-in one,
    // which would otherwise win over Shoal's.
    for (config, user, rest) in [
        ("", &[][..], &[][..]),
        ("model_provider = 'openai'\n", &[][..], &[][..]),
        (
            "model_provider = 'cliproxy'\n",
            &["-c", "model_provider=openai", "exec"][..],
            &["exec"][..],
        ),
    ] {
        let args = routed(config, user).unwrap();
        assert_eq!(args[1..4], ["-c", shoal, "-c"], "{args:?}");
        assert!(
            args[4].contains("requires_openai_auth=true")
                && args[4].contains(&format!("base_url=\"{URL}\"")),
            "{args:?}"
        );
        assert_eq!(args[5..], *rest, "{args:?}");
    }
    // A profile on it is pointed at Shoal's provider where it names it.
    let config = "[profiles.\"my work\"]\nmodel_provider = 'openai'\n";
    let args = routed(config, &["-p", "my work"]).unwrap();
    assert_eq!(
        args[2],
        "profiles.\"my work\".model_provider=\"shoal-proxy\""
    );
    assert_eq!(args[5..], ["-p", "my work"]);
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
