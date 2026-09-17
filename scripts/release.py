#!/usr/bin/env python3
"""Prepare a release PR, then publish its merged version using the fj login."""
import argparse
from pathlib import Path
import re
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parent.parent


def run(*args, capture=False):
    return subprocess.run(args, cwd=ROOT, check=True, text=True,
                          stdout=subprocess.PIPE if capture else None).stdout


def git(*args):
    return run("git", *args, capture=True).strip()


def parts(version):
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", version):
        raise ValueError("use MAJOR.MINOR.PATCH without v or a prerelease suffix")
    return tuple(map(int, version.split(".")))


def versions(manifest, lock):
    package = tomllib.loads(manifest)["package"]
    local = [p for p in tomllib.loads(lock)["package"]
             if p["name"] == "shoal" and "source" not in p]
    if package["name"] != "shoal" or len(local) != 1 or local[0]["version"] != package["version"]:
        raise ValueError("expected matching local Shoal versions in Cargo.toml and Cargo.lock")
    parts(package["version"])
    return package["version"]


def bump(manifest, lock, requested, occupied):
    current = versions(manifest, lock)
    major, minor, patch = parts(current)
    if not requested:
        patch += 1
        while f"v{major}.{minor}.{patch}" in occupied:
            patch += 1
        requested = f"{major}.{minor}.{patch}"
    if parts(requested) <= parts(current):
        raise ValueError(f"version must be newer than {current}")
    if f"v{requested}" in occupied:
        raise ValueError("version already has a tag or release branch")
    manifest, count = re.subn(r'(\[package\][\s\S]*?^version\s*=\s*)"[^"]+"',
                             lambda m: m[1] + f'"{requested}"', manifest, count=1, flags=re.M)
    lock, lock_count = re.subn(r'(\[\[package\]\]\nname = "shoal"\nversion = )"[^"]+"',
                              lambda m: m[1] + f'"{requested}"', lock, count=1)
    if count != 1 or lock_count != 1 or versions(manifest, lock) != requested:
        raise ValueError("could not safely update package versions")
    return requested, manifest, lock


def validate():
    run("cargo", "fmt", "--check")
    run("cargo", "clippy", "--locked", "--all-targets", "--", "-D", "warnings")
    run("cargo", "test", "--locked")
    run("python3", "-m", "unittest", "discover", "-s", "scripts", "-p", "test_*.py")
    run("cargo", "build", "--locked", "--release")


def prepare(requested, dry_run):
    if git("status", "--porcelain"):
        raise ValueError("commit or stash changes before preparing a release")
    run("git", "fetch", "origin", "+refs/heads/*:refs/remotes/origin/*", "--tags")
    if git("rev-parse", "HEAD") != git("rev-parse", "origin/main"):
        raise ValueError("start from the current origin/main commit")
    occupied = set(git("tag", "--list", "v*").splitlines())
    for ref in git("for-each-ref", "--format=%(refname)", "refs/heads/release/",
                   "refs/remotes/origin/release/").splitlines():
        occupied.add(ref.split("/release/", 1)[1])
    version, manifest, lock = bump((ROOT / "Cargo.toml").read_text(),
                                   (ROOT / "Cargo.lock").read_text(), requested, occupied)
    if dry_run:
        print(f"Would prepare release/v{version} from origin/main")
        return
    run("fj", "whoami")
    branch = f"release/v{version}"
    run("git", "switch", "-c", branch)
    (ROOT / "Cargo.toml").write_text(manifest)
    (ROOT / "Cargo.lock").write_text(lock)
    validate()
    run("git", "add", "Cargo.toml", "Cargo.lock")
    run("git", "commit", "-m", f"chore: prepare release v{version}")
    run("git", "push", "-u", "origin", branch)
    with tempfile.NamedTemporaryFile(mode="w", suffix=".md") as body:
        body.write(f"Prepare Shoal {version}. Both Cargo versions are updated.\n\n"
                   "Validated formatting, Clippy, tests, and the release build.\n\n"
                   f"After merging, run `python3 scripts/release.py publish {version}`. "
                   "Publishing the release automatically updates the shared Homebrew tap.\n")
        body.flush()
        run("fj", "pr", "create", f"Release v{version}", "--base", "main",
            "--head", branch, "--body-file", body.name)


def publish(version, dry_run):
    parts(version)
    if git("status", "--porcelain"):
        raise ValueError("commit or stash changes before publishing a release")
    run("git", "fetch", "origin", "+refs/heads/main:refs/remotes/origin/main", "--tags")
    sha = git("rev-parse", "origin/main")
    # Validate what will actually be tagged, not the caller's old checkout.
    if git("rev-parse", "HEAD") != sha:
        raise ValueError("check out the current origin/main commit before publishing")
    if versions(git("show", f"{sha}:Cargo.toml"), git("show", f"{sha}:Cargo.lock")) != version:
        raise ValueError("requested version is not merged into main")
    tag = f"v{version}"
    existing = git("tag", "--list", tag)
    if existing and git("rev-parse", f"refs/tags/{tag}^{{commit}}") != sha:
        raise ValueError("release tag already points elsewhere; it will not be moved")
    if dry_run:
        print(f"Would publish {tag} at {sha}; Homebrew updates through Actions")
        return
    run("fj", "whoami")
    validate()
    if not existing:
        run("git", "tag", "-a", tag, sha, "-m", f"Shoal {version}")
    run("git", "push", "origin", f"refs/tags/{tag}")
    # fj returns an error if the release already exists: never overwrite it.
    run("fj", "release", "create", f"Shoal {version}", "--tag", tag,
        "--body", f"Shoal {version}.\n\nInstall or upgrade through the "
        "[HN05 Homebrew tap](https://github.com/HN05/homebrew-tap).")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    prep = commands.add_parser("prepare", help="bump versions, validate, and open a release PR")
    prep.add_argument("version", nargs="?")
    pub = commands.add_parser("publish", help="validate merged main, tag, and create a Forgejo release")
    pub.add_argument("version")
    for command in (prep, pub):
        command.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    try:
        (prepare if args.command == "prepare" else publish)(args.version, args.dry_run)
    except (ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Release failed: {error}\n")


if __name__ == "__main__":
    main()
