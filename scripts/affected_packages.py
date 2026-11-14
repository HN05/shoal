"""The workspace packages a branch's commits can affect: those whose directory
they change and every package that depends on one of them.

Prints `all` when every package is affected, or when Git finds no merge base
with the base commit, else the name of each affected package on a line of its
own, and nothing when the commits change no package. A file a package compiles or its
tests read lives in its directory, or in that of a package it depends on, so
a changed path names the package it affects; scripts/check_package_sources.py
holds CI's builds to that."""
import argparse
import json
import subprocess
import sys
from dataclasses import dataclass
from pathlib import PurePosixPath

ALL = "all"
# Paths every package's build, or CI's choice of what to build, reads; one
# ending in `/` is a directory.
WORKSPACE_INPUTS = (
    "Cargo.toml",
    "Cargo.lock",
    ".cargo/",
    ".config/",
    "clippy.toml",
    ".shoal.toml",
    ".shoal/",
    ".forgejo/workflows/ci.yml",
    ".forgejo/scripts/check-toolchain.sh",
    ".forgejo/scripts/lock-target-dir.sh",
    "scripts/affected_packages.py",
    "scripts/check_package_sources.py",
)
# The root package's directory is the workspace's, so it owns only these.
ROOT_PACKAGE_DIRECTORIES = ("src/", "tests/")


def under(path, prefix):
    return path.startswith(prefix) if prefix.endswith("/") else path == prefix


@dataclass(frozen=True)
class Workspace:
    # Each package's directories, relative to the workspace root and ending
    # in `/`.
    directories: dict
    # Each package's dependents in the workspace, through normal, build and
    # dev dependencies alike.
    dependents: dict

    @classmethod
    def from_metadata(cls, metadata):
        """The workspace `cargo metadata --no-deps` describes, its packages
        linked by the path dependencies they declare."""
        root = PurePosixPath(metadata["workspace_root"])
        members = set(metadata["workspace_members"])
        packages = [package for package in metadata["packages"] if package["id"] in members]
        names = {package["name"] for package in packages}
        directories = {}
        dependents = {name: set() for name in names}
        for package in packages:
            directory = PurePosixPath(package["manifest_path"]).parent.relative_to(root)
            directories[package["name"]] = (
                ROOT_PACKAGE_DIRECTORIES if directory == PurePosixPath(".")
                else (f"{directory}/",))
            for dependency in package["dependencies"]:
                if dependency.get("path") and dependency["name"] in names:
                    dependents[dependency["name"]].add(package["name"])
        return cls(directories, dependents)

    def owner(self, path):
        """The package whose directory holds *path*, or None."""
        return next((name for name, owned in self.directories.items()
                     if any(path.startswith(directory) for directory in owned)), None)

    def affected(self, paths):
        """`ALL`, or the sorted names of the packages that changes to *paths*
        can affect."""
        selected = set()
        for path in paths:
            if any(under(path, prefix) for prefix in WORKSPACE_INPUTS):
                return ALL
            owner = self.owner(path)
            if owner is not None:
                selected.add(owner)
        pending = list(selected)
        while pending:
            for dependent in self.dependents[pending.pop()] - selected:
                selected.add(dependent)
                pending.append(dependent)
        return ALL if selected == set(self.directories) else sorted(selected)


def cargo_metadata():
    """The workspace's `cargo metadata --no-deps`."""
    metadata = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        check=True, stdout=subprocess.PIPE, text=True).stdout
    return json.loads(metadata)


def changed_paths(base, cwd=None, env=None):
    """The paths the commits since HEAD's merge base with *base* change, both
    names of a renamed file among them, or None when Git has no such merge
    base, as when *base* is missing from a shallow clone."""
    merge_base = subprocess.run(["git", "merge-base", base, "HEAD"],
                                cwd=cwd, env=env, capture_output=True, text=True)
    if merge_base.returncode != 0:
        return None
    diff = subprocess.run(
        ["git", "diff", "--name-only", "--no-renames", "-z", merge_base.stdout.strip(), "HEAD"],
        cwd=cwd, env=env, check=True, capture_output=True, text=True).stdout
    return [path for path in diff.split("\0") if path]


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("base", help="the commit the branch is compared with, such as origin/main")
    arguments = parser.parse_args()
    paths = changed_paths(arguments.base)
    if paths is None:
        print(f"no merge base with {arguments.base}: every package is affected", file=sys.stderr)
        print(ALL)
        return
    affected = Workspace.from_metadata(cargo_metadata()).affected(paths)
    for name in [ALL] if affected == ALL else affected:
        print(name)


if __name__ == "__main__":
    main()
