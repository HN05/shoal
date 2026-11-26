"""Fail when a workspace package declares a dependency its Rust sources never
name, unless its manifest lists it under `[package.metadata.shoal]`, as
AGENTS.md, under Crates, says.

A source names a dependency by a path that starts with its crate name, such
as `hyper::Request`, or by `use` or `extern crate`."""
import re
import sys
from pathlib import Path

from affected_packages import Workspace, cargo_metadata

METADATA_KEY = "unnamed-dependencies"


def crate_name(dependency):
    """The name *dependency* has in Rust code."""
    return (dependency.get("rename") or dependency["name"]).replace("-", "_")


def names(source, crate):
    """Whether Rust *source* names *crate*."""
    return re.search(rf"(?<!\w)(?<!\w::){crate}\s*::|\b(?:use|extern\s+crate)\s+{crate}\b",
                     source) is not None


def unnamed_dependencies(dependencies, sources, allowed):
    """The sorted crate names among *dependencies* that no text in *sources*
    names and *allowed* leaves out."""
    exempt = {crate.replace("-", "_") for crate in allowed}
    return sorted({crate for crate in map(crate_name, dependencies)
                   if crate not in exempt and not any(names(source, crate) for source in sources)})


def stale_allowances(dependencies, allowed):
    """The names in *allowed* that are no dependency in *dependencies*."""
    declared = {crate_name(dependency) for dependency in dependencies}
    return sorted(crate for crate in allowed if crate.replace("-", "_") not in declared)


def rust_sources(root, directories):
    """The text of every Rust file under *directories* of the workspace at
    *root*, leaving out build output."""
    return [path.read_text() for directory in directories
            for path in sorted((Path(root) / directory).rglob("*.rs"))
            if "target" not in path.relative_to(root).parts]


def problems(metadata):
    """(package, problem) for each unnamed dependency and stale allowance."""
    workspace = Workspace.from_metadata(metadata)
    members = set(metadata["workspace_members"])
    return sorted((package["name"], problem) for package in metadata["packages"]
                  if package["id"] in members
                  for problem in package_problems(
                      package, rust_sources(metadata["workspace_root"],
                                            workspace.directories[package["name"]])))


def allowances(package):
    """The dependencies *package*'s manifest lists as named by no source."""
    return ((package.get("metadata") or {}).get("shoal") or {}).get(METADATA_KEY, [])


def package_problems(package, sources):
    """Each problem of *package*, whose Rust files hold *sources*."""
    allowed = allowances(package)
    return ([f"declares `{crate}`, which its sources never name: remove it, or list it in "
             f"[package.metadata.shoal] {METADATA_KEY} with a comment that says what needs it"
             for crate in unnamed_dependencies(package["dependencies"], sources, allowed)]
            + [f"lists `{crate}` in {METADATA_KEY}, which is none of its dependencies: "
               "remove it from the list"
               for crate in stale_allowances(package["dependencies"], allowed)])


def main():
    found = problems(cargo_metadata())
    for package, problem in found:
        print(f"::error::{package} {problem}", file=sys.stderr)
    if found:
        sys.exit(1)


if __name__ == "__main__":
    main()
