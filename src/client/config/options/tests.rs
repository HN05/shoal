use super::*;
use serde::Deserialize;

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default)]
#[options(resolved = Example)]
struct ExampleConfig {
    plain: Option<u32>,
    #[option(default = 7)]
    count: Option<u32>,
    #[option(named)]
    named: BTreeMap<String, u32>,
    #[option(list)]
    list: Vec<String>,
    #[option(template, default = "base".into())]
    text: Option<String>,
    #[option(section)]
    inner: InnerConfig,
    #[option(default = 2, resolve = doubled -> u64)]
    converted: Option<u32>,
    #[serde(flatten)]
    #[option(named)]
    rest: BTreeMap<String, String>,
}

fn doubled(value: u32, config: &ExampleConfig) -> Result<u64> {
    Ok(u64::from(value) * 2 + u64::from(config.plain.unwrap_or_default()))
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Self)]
struct InnerConfig {
    #[serde(rename = "flag")]
    enabled: Option<bool>,
}

fn layer(text: &str) -> ExampleConfig {
    toml::from_str(text).unwrap()
}

fn merged(global: &str, repository: &str) -> ExampleConfig {
    let mut base = ExampleConfig::built_in();
    base.merge(layer(global), Merge::SameLevel);
    base.merge(layer(repository), Merge::RepositoryOverGlobal);
    base
}

#[test]
fn the_built_in_layer_resolves_to_its_defaults() {
    let resolved = ExampleConfig::built_in().resolve("").unwrap();
    assert_eq!(resolved.plain, None);
    assert_eq!(resolved.count, 7);
    assert_eq!(resolved.text, "base");
    assert_eq!(resolved.converted, 4);
    assert!(resolved.named.is_empty() && resolved.list.is_empty());
}

#[test]
fn each_kind_layers_by_its_rule() {
    let resolved = merged(
        "plain = 1\ncount = 2\nlist = ['a']\ntext = 'global'\nother = 'x'\n[named]\na = 1\nb = 2\n[inner]\nflag = true",
        "count = 3\nlist = []\ntext = 'mine'\n[named]\nb = 20\n",
    )
    .resolve("")
    .unwrap();
    assert_eq!((resolved.plain, resolved.count), (Some(1), 3));
    assert_eq!(
        resolved.named,
        BTreeMap::from([("a".into(), 1), ("b".into(), 20)])
    );
    assert_eq!(resolved.list, ["a"]);
    assert_eq!(resolved.text, "global\n\nmine");
    assert_eq!(resolved.inner.enabled, Some(true));
    assert_eq!(resolved.converted, 5);
    assert_eq!(resolved.rest["other"], "x");
}

#[test]
fn a_missing_default_names_its_key() {
    let mut base = ExampleConfig::built_in();
    base.count = None;
    let error = base.resolve("example").unwrap_err().to_string();
    assert_eq!(error, "example.count has no built-in default");
}

#[test]
fn provenance_reports_the_layer_in_effect_under_each_key() {
    let built_in = ExampleConfig::built_in();
    let global = layer("count = 2\nother = 'x'\n[inner]\nflag = false");
    let repository = layer("text = 'mine'\n[named]\na = 1");
    let sources = [
        (&repository, Layer::WorktreeFile),
        (&global, Layer::GlobalConfig),
        (&built_in, Layer::BuiltInDefault),
    ];
    let mut entries = Vec::new();
    ExampleConfig::entries("", &sources, &mut entries).unwrap();
    let found = |key: &str| {
        let entry = entries.iter().find(|entry| entry.key == key).unwrap();
        (entry.value.clone(), entry.layer)
    };
    assert_eq!(found("plain"), (Value::Null, Layer::BuiltInDefault));
    assert_eq!(found("count"), (2.into(), Layer::GlobalConfig));
    assert_eq!(found("text"), ("base\n\nmine".into(), Layer::WorktreeFile));
    assert_eq!(found("named.a"), (1.into(), Layer::WorktreeFile));
    assert_eq!(found("inner.flag"), (false.into(), Layer::GlobalConfig));
    assert_eq!(found("other"), ("x".into(), Layer::GlobalConfig));
}

#[test]
fn templates_append_only_across_levels() {
    let mut base = Some("base".to_owned());
    template(&mut base, Some("global".into()), Merge::SameLevel);
    assert_eq!(base.as_deref(), Some("global"));
    template(&mut base, Some("mine".into()), Merge::RepositoryOverGlobal);
    assert_eq!(base.as_deref(), Some("global\n\nmine"));
    let mut empty = Some(String::new());
    template(&mut empty, Some("mine".into()), Merge::RepositoryOverGlobal);
    assert_eq!(empty.as_deref(), Some("mine"));
    let mut kept = Some("global".to_owned());
    template(&mut kept, Some(String::new()), Merge::RepositoryOverGlobal);
    assert_eq!(kept.as_deref(), Some("global"));
}
