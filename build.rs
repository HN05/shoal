use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=configs");
    let directory = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("configs");
    let mut configs = Vec::new();
    for entry in fs::read_dir(directory).expect("read packaged configs") {
        let path = entry.expect("read config entry").path();
        if path.extension().is_none_or(|extension| extension != "toml") {
            continue;
        }
        assert!(path.is_file(), "config must be a file: {}", path.display());
        let name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .expect("config name must be UTF-8")
            .to_owned();
        assert!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
            "config names must contain only ASCII letters, digits, hyphens or underscores: {name}"
        );
        configs.push((name, path));
    }
    configs.sort_by(|a, b| a.0.cmp(&b.0));
    let mut source = String::from("&[\n");
    for (name, path) in configs {
        source.push_str(&format!(
            "({name:?}, include_str!({:?})),\n",
            path.to_str().unwrap()
        ));
    }
    source.push_str("]\n");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("packaged_configs.rs");
    fs::write(output, source).expect("write packaged config registry");
}
