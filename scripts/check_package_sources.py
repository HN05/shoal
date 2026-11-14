"""Fail when a test binary was built from a file outside its package's
directory and those of the packages it depends on.

scripts/affected_packages.py names the packages a change affects by the
directories it touches, so a change to such a file would select none of the
packages built from it. Cargo's dep-info, beside each binary
`cargo nextest list` names, lists the files the build read."""
import json
import os
import re
import subprocess
import sys
from pathlib import Path

from affected_packages import ALL, Workspace, cargo_metadata


def dep_info_paths(text):
    """The files a Make-style dep-info file lists as prerequisites."""
    paths = set()
    for line in text.splitlines():
        if line.startswith("#"):
            continue
        _target, separator, prerequisites = line.partition(": ")
        if separator:
            paths.update(path.replace("\\ ", " ")
                         for path in re.split(r"(?<!\\) ", prerequisites) if path)
    return paths


def workspace_path(path, root, generated):
    """*path* relative to the workspace *root*, where rustc's own relative
    paths start, or None for a file outside it or in one of the *generated*
    build directories."""
    path = os.path.normpath(os.path.join(root, path))
    if os.path.commonpath([path, root]) != root or any(
            os.path.commonpath([path, directory]) == directory for directory in generated):
        return None
    return os.path.relpath(path, root)


def stray_sources(workspace, root, generated, package, dep_info):
    """The workspace files in *dep_info* that a change to would not select
    *package*."""
    stray = []
    for path in sorted(dep_info_paths(dep_info)):
        relative = workspace_path(path, root, generated)
        if relative is not None:
            affected = workspace.affected([relative])
            if affected != ALL and package not in affected:
                stray.append(relative)
    return stray


def test_binaries():
    """Each test binary's package and path, from `cargo nextest list`."""
    listing = subprocess.run(
        ["cargo", "nextest", "list", "--locked", "--workspace", "--message-format", "json"],
        check=True, stdout=subprocess.PIPE, text=True).stdout
    return [(suite["package-name"], suite["binary-path"])
            for suite in json.loads(listing)["rust-suites"].values()]


def main():
    metadata = cargo_metadata()
    workspace = Workspace.from_metadata(metadata)
    root = metadata["workspace_root"]
    generated = {metadata["target_directory"], metadata.get("build_directory",
                                                            metadata["target_directory"])}
    failed = False
    for package, binary in test_binaries():
        dep_info = Path(binary + ".d")
        if not dep_info.is_file():
            print(f"::error::{package}: no dep-info at {dep_info}", file=sys.stderr)
            failed = True
            continue
        for path in stray_sources(workspace, root, generated, package, dep_info.read_text()):
            print(f"::error::{package} builds {binary} from {path}, outside its directory and "
                  "those of the packages it depends on: move the file into the package "
                  "(AGENTS.md, Implementation)", file=sys.stderr)
            failed = True
    if failed:
        sys.exit(1)


if __name__ == "__main__":
    main()
