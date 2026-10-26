//! Xcode's DerivedData folders. Xcode keeps each project's build in its own
//! folder, whose `info.plist` records the project in `WorkspacePath`; folders
//! without one, such as the shared module caches, are never deleted.
use std::{
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

const PREFERENCES: &str = "Library/Preferences/com.apple.dt.Xcode.plist";
const DEFAULT_LOCATION: &str = "Library/Developer/Xcode/DerivedData";

/// Xcode's DerivedData directory: its custom location when that is absolute,
/// otherwise the default. A relative location keeps DerivedData beside each
/// project, where it goes with the worktree, so there is nothing to find.
pub fn location(home: &Path) -> Option<PathBuf> {
    let custom = plist::Value::from_file(home.join(PREFERENCES))
        .ok()
        .and_then(|preferences| {
            let location = preferences
                .as_dictionary()?
                .get("IDECustomDerivedDataLocation")?
                .as_string()?;
            Some(PathBuf::from(location))
        });
    match custom {
        Some(location) if location.is_absolute() => Some(location),
        Some(_) => None,
        None => Some(home.join(DEFAULT_LOCATION)),
    }
}

/// Delete every project folder in `location` whose project `deletes` selects,
/// returning the deleted folders. A missing location has none.
pub fn delete(location: &Path, deletes: impl Fn(&Path) -> bool) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(location) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        entries => entries.with_context(|| format!("read {}", location.display()))?,
    };
    let mut deleted = Vec::new();
    for entry in entries {
        let folder = entry?.path();
        if project(&folder).is_some_and(|project| deletes(&project)) {
            fs::remove_dir_all(&folder).with_context(|| format!("delete {}", folder.display()))?;
            deleted.push(folder);
        }
    }
    Ok(deleted)
}

/// The project a DerivedData folder belongs to, if it is a project's folder.
fn project(folder: &Path) -> Option<PathBuf> {
    if !fs::symlink_metadata(folder).ok()?.is_dir() {
        return None;
    }
    let info = plist::Value::from_file(folder.join("info.plist")).ok()?;
    let path = info.as_dictionary()?.get("WorkspacePath")?.as_string()?;
    Some(PathBuf::from(path))
}

/// A project's DerivedData folder named `name` in `location`.
#[cfg(test)]
pub fn folder(location: &Path, name: &str, project: &Path) -> PathBuf {
    let folder = location.join(name);
    fs::create_dir_all(folder.join("Build")).unwrap();
    let info = plist::Dictionary::from_iter([(
        "WorkspacePath".to_owned(),
        plist::Value::String(project.display().to_string()),
    )]);
    plist::Value::Dictionary(info)
        .to_file_xml(folder.join("info.plist"))
        .unwrap();
    folder
}

#[cfg(test)]
mod tests;
