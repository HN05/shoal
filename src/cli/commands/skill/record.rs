//! What Shoal installed in one skill directory, which decides what it still owns.
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fsutil::{self, Permissions, ReplaceOptions};

/// A dotfile beside the skills, which AI tools do not load as a skill.
const FILE: &str = ".shoal-skills.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Entry {
    /// A symlink to the packaged skill.
    Link(PathBuf),
    /// A copy of the embedded skill, by the SHA-256 of its contents.
    Sha256(String),
    /// Changed or removed by the user; only an explicit install replaces it.
    Kept,
}

impl Entry {
    pub(super) fn copy(contents: &[u8]) -> Self {
        let digest = Sha256::digest(contents);
        Self::Sha256(digest.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    /// Whether `path` is exactly what this entry installed.
    pub(super) fn matches(&self, path: &Path) -> Result<bool> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if is_absent(&error) => return Ok(false),
            Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
        };
        Ok(match self {
            Self::Link(target) => {
                metadata.is_symlink()
                    && fs::read_link(path).with_context(|| format!("read {}", path.display()))?
                        == *target
            }
            Self::Sha256(_) => {
                metadata.is_file()
                    && Self::copy(
                        &fs::read(path).with_context(|| format!("read {}", path.display()))?,
                    ) == *self
            }
            Self::Kept => false,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub(super) skills: BTreeMap<String, Entry>,
}

impl Record {
    /// `None` when Shoal has not recorded an installation in `directory`.
    pub(super) fn load(directory: &Path) -> Result<Option<Self>> {
        let path = directory.join(FILE);
        fsutil::read_optional(&path)
            .with_context(|| format!("read {}", path.display()))?
            .map(|text| {
                let record: Self = serde_json::from_str(&text)?;
                // Names become paths that refresh may remove.
                for name in record.skills.keys() {
                    crate::validate::name("skill", name)?;
                }
                anyhow::Ok(record)
            })
            .transpose()
            .with_context(|| format!("parse {}; remove it to reinstall", path.display()))
    }

    pub(super) fn save(&self, directory: &Path) -> Result<()> {
        let path = directory.join(FILE);
        fsutil::replace_atomically(
            &path,
            &serde_json::to_vec_pretty(self)?,
            ReplaceOptions {
                permissions: Permissions::Mode(0o644),
                sync: false,
            },
        )
        .with_context(|| format!("write {}", path.display()))
    }
}

/// Whether an error means the path does not exist, including below a file.
pub(super) fn is_absent(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn entries_match_only_what_they_installed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("SKILL.md");
        let copy = Entry::copy(b"skill");
        let link = Entry::Link(root.path().join("packaged.md"));
        assert!(!copy.matches(&path).unwrap());
        fs::write(&path, "skill").unwrap();
        assert!(copy.matches(&path).unwrap());
        assert!(!link.matches(&path).unwrap());
        assert!(!Entry::Kept.matches(&path).unwrap());
        fs::write(&path, "edited").unwrap();
        assert!(!copy.matches(&path).unwrap());
        fs::remove_file(&path).unwrap();
        fs::write(root.path().join("same.md"), "skill").unwrap();
        // A link to identical contents is not the copy Shoal wrote.
        symlink(root.path().join("same.md"), &path).unwrap();
        assert!(!copy.matches(&path).unwrap());
        assert!(!link.matches(&path).unwrap());
        fs::remove_file(&path).unwrap();
        symlink(root.path().join("packaged.md"), &path).unwrap();
        assert!(link.matches(&path).unwrap());
    }

    #[test]
    fn record_round_trips_and_rejects_unknown_contents() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(Record::load(root.path()).unwrap(), None);
        let record = Record {
            skills: BTreeMap::from([
                ("copy".to_owned(), Entry::copy(b"skill")),
                ("link".to_owned(), Entry::Link("/skills/SKILL.md".into())),
                ("kept".to_owned(), Entry::Kept),
            ]),
        };
        record.save(root.path()).unwrap();
        assert_eq!(Record::load(root.path()).unwrap(), Some(record));
        for invalid in [
            r#"{"skills":{"x":"other"}}"#,
            r#"{"skills":{"../outside":"kept"}}"#,
            r#"{"skills":{"nested/skill":"kept"}}"#,
        ] {
            fs::write(root.path().join(FILE), invalid).unwrap();
            assert!(Record::load(root.path()).is_err(), "{invalid}");
        }
    }
}
