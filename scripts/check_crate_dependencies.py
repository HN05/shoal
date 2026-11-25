"""Fail when a workspace package depends on a workspace package that its entry
in scripts/crate_dependencies.toml does not allow, or when the file's entries
and the workspace's packages differ.

`cargo metadata --no-deps` lists every dependency a manifest declares,
optional and target-specific ones among them, so a dependency a feature
enables counts as any other."""
import sys
import tomllib
from pathlib import Path

from affected_packages import cargo_metadata

ALLOWED = Path(__file__).with_name("crate_dependencies.toml")
ENTRY_KEYS = ("dependencies", "dev-dependencies")


def declared_dependencies(metadata):
    """Each workspace package's dependencies on workspace packages, as sorted
    (dependency, kind) pairs, where kind is `normal`, `build` or `dev`."""
    members = set(metadata["workspace_members"])
    packages = [package for package in metadata["packages"] if package["id"] in members]
    names = {package["name"] for package in packages}
    return {package["name"]: sorted({(dependency["name"], dependency["kind"] or "normal")
                                     for dependency in package["dependencies"]
                                     if dependency["name"] in names})
            for package in packages}


def entry_problems(allowed, packages):
    """(package, problem) for each of *packages* without an entry in
    *allowed*, and each entry that names something other than a package or
    has a key the file does not define."""
    problems = [(package, "has no entry") for package in sorted(set(packages) - set(allowed))]
    for package, entry in sorted(allowed.items()):
        if package not in packages:
            problems.append((package, "has an entry but is no workspace package"))
        for key in sorted(set(entry) - set(ENTRY_KEYS)):
            problems.append((package, f"has an unknown key `{key}`"))
        for key in ENTRY_KEYS:
            for name in entry.get(key, []):
                if name not in packages:
                    problems.append((package, f"allows `{name}`, which is no workspace package"))
    return problems


def allows(entry, dependency, kind):
    """Whether *entry* lets its package depend on *dependency* as *kind*."""
    allowed = list(entry.get("dependencies", []))
    if kind == "dev":
        allowed += entry.get("dev-dependencies", [])
    return dependency in allowed


def dependency_problems(allowed, declared):
    """(package, problem) for each dependency in *declared* that the
    package's entry in *allowed* does not allow."""
    return [(package, f"has a {kind} dependency on `{dependency}`, which its entry does not allow")
            for package, dependencies in sorted(declared.items()) if package in allowed
            for dependency, kind in dependencies
            if not allows(allowed[package], dependency, kind)]


def problems(allowed, declared):
    return entry_problems(allowed, set(declared)) + dependency_problems(allowed, declared)


def main():
    found = problems(tomllib.loads(ALLOWED.read_text()), declared_dependencies(cargo_metadata()))
    for package, problem in found:
        print(f"::error::{package} {problem} in scripts/{ALLOWED.name}; AGENTS.md, under "
              "Implementation, gives the principles each entry follows", file=sys.stderr)
    if found:
        sys.exit(1)


if __name__ == "__main__":
    main()
