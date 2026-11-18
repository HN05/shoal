//! The `git init`, `git add` and `git commit` the fixtures run, done in this
//! process through gix, since tests build thousands of repositories. Each
//! leaves the repository as Git would: the index matches the commit, with
//! the files' stat data, so Git reads the worktree as clean without
//! refreshing the index.
use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

use gix::bstr::{BStr, ByteSlice as _};
use gix_index::entry::{Flags, Mode, Stage, Stat};

/// Create a repository with a worktree in the empty directory `repo`, on
/// `main`, with only its own configuration.
pub(super) fn init(repo: &Path) {
    gix::ThreadSafeRepository::init_opts(
        repo,
        gix::create::Kind::WithWorktree,
        gix::create::Options::default(),
        gix::open::Options::isolated(),
    )
    .unwrap();
}

/// Stage each of `paths` in the worktree `repo`, as `git add` does, and
/// commit the index on HEAD with `message`, under `identity`. Answer
/// `false`, changing nothing, where Git would do more: for a directory,
/// which it stages whole, or during a merge, rebase, cherry-pick or revert,
/// which the commit concludes.
pub(super) fn commit(repo: &Path, paths: &[&str], message: &str, identity: [&str; 2]) -> bool {
    let repo = gix::open_opts(repo, gix::open::Options::isolated()).unwrap();
    if !commits_as_git(&repo, paths) {
        return false;
    }
    let tree = staged_tree(&repo, paths);
    commit_tree(&repo, tree, message, identity);
    true
}

/// Whether committing `paths` in `repo` takes Git only what this module
/// does: no directory, and no operation in progress.
fn commits_as_git(repo: &gix::Repository, paths: &[&str]) -> bool {
    let workdir = repo.workdir().unwrap();
    let is_dir =
        |path: &&str| fs::symlink_metadata(workdir.join(path)).is_ok_and(|file| file.is_dir());
    repo.state().is_none() && !paths.iter().any(is_dir)
}

/// Stage `paths` in `repo`'s index and write the tree it then holds.
fn staged_tree(repo: &gix::Repository, paths: &[&str]) -> gix::ObjectId {
    let mut index = index(repo);
    for path in paths {
        stage(repo, &mut index, path);
    }
    index.remove_tree();
    index.write(gix_index::write::Options::default()).unwrap();
    tree(repo, &index)
}

/// Commit `tree` on HEAD with `message` as `identity` at this moment, as
/// `git commit` does, refusing a commit that changes nothing.
fn commit_tree(repo: &gix::Repository, tree: gix::ObjectId, message: &str, identity: [&str; 2]) {
    let parent = repo.head_commit().ok();
    if let Some(parent) = &parent {
        assert_ne!(parent.tree_id().unwrap(), tree, "nothing to commit");
    }
    let signature = signature_now(identity);
    let mut time = gix::date::parse::TimeBuf::default();
    let signature = signature.to_ref(&mut time);
    let message = format!("{message}\n");
    let parents = parent.map(|parent| parent.id);
    repo.commit_as(signature, signature, "HEAD", message, tree, parents)
        .unwrap();
}

/// `identity`, a name and an email, signing at this moment.
fn signature_now([name, email]: [&str; 2]) -> gix::actor::Signature {
    gix::actor::Signature {
        name: name.into(),
        email: email.into(),
        time: gix::date::Time::now_local_or_utc(),
    }
}

/// The index of `repo`'s worktree, empty before its first commit.
fn index(repo: &gix::Repository) -> gix_index::File {
    let options = gix_index::decode::Options::default();
    gix_index::File::at_or_default(repo.index_path(), repo.object_hash(), false, options).unwrap()
}

/// Record the file at `path` in `index` with its contents, mode and stat
/// data, or drop it from the index where the worktree no longer has it.
fn stage(repo: &gix::Repository, index: &mut gix_index::File, path: &str) {
    let file = repo.workdir().unwrap().join(path);
    let key = path.as_bytes().as_bstr();
    match blob(&file) {
        None => index.remove_entries(|_, entry_path, _| entry_path == key),
        Some((mode, contents)) => {
            let id = repo.write_blob(contents).unwrap().detach();
            let stat = gix_index::fs::Metadata::from_path_no_follow(&file).unwrap();
            let stat = Stat::from_fs(&stat).unwrap();
            set_entry(index, key, (mode, id, stat));
        }
    }
}

/// The mode and contents Git stores for the file or symlink at `file`, or
/// `None` where there is nothing.
fn blob(file: &Path) -> Option<(Mode, Vec<u8>)> {
    let metadata = match fs::symlink_metadata(file) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        metadata => metadata.unwrap(),
    };
    if metadata.is_symlink() {
        let target = fs::read_link(file).unwrap();
        return Some((Mode::SYMLINK, target.into_os_string().into_encoded_bytes()));
    }
    assert!(metadata.is_file(), "{} is not a file", file.display());
    let executable = metadata.permissions().mode() & 0o100 != 0;
    let mode = if executable {
        Mode::FILE_EXECUTABLE
    } else {
        Mode::FILE
    };
    Some((mode, fs::read(file).unwrap()))
}

/// Set `index`'s entry for `path` to `mode`, `id` and `stat`, adding it
/// where the index has none.
fn set_entry(
    index: &mut gix_index::File,
    path: &BStr,
    (mode, id, stat): (Mode, gix::ObjectId, Stat),
) {
    match index.entry_mut_by_path_and_stage(path, Stage::Unconflicted) {
        Some(entry) => {
            entry.stat = stat;
            entry.id = id;
            entry.mode = mode;
        }
        None => {
            index.dangerously_push_entry(stat, id, Flags::empty(), mode, path);
            index.sort_entries();
        }
    }
}

/// Write the tree `index` holds, as `git write-tree` does.
fn tree(repo: &gix::Repository, index: &gix_index::File) -> gix::ObjectId {
    let mut editor = repo.edit_tree(repo.empty_tree().id).unwrap();
    for entry in index.entries() {
        assert_eq!(entry.stage(), Stage::Unconflicted, "unmerged entries");
        let kind = entry.mode.to_tree_entry_mode().unwrap().kind();
        editor
            .upsert(entry.path(index).to_owned(), kind, entry.id)
            .unwrap();
    }
    editor.write().unwrap().detach()
}
