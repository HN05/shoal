#!/usr/bin/env python3
"""Prepare a release PR, then publish its merged version using the fj login."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import tomllib
from urllib.error import HTTPError
from urllib.request import Request, urlopen
import release_notes
from release_metadata import commit_id, pull_branch, tag_version, version_parts as parts, version_tag

ROOT = Path(__file__).resolve().parent.parent


def run(*args, capture=False):
    return subprocess.run(args, cwd=ROOT, check=True, text=True,
                          stdout=subprocess.PIPE if capture else None).stdout


def git(*args):
    return run("git", *args, capture=True).strip()


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


def api(path, data=None, authenticated=True):
    base = os.environ.get("RELEASE_API_URL", "https://git.henriknordvik.com/api/v1").rstrip("/")
    token = os.environ.get("RELEASE_AUTOMATION_TOKEN")
    headers = {"Content-Type": "application/json"}
    if token and authenticated:
        headers["Authorization"] = f"token {token}"
    request = Request(base + path, data=json.dumps(data).encode() if data is not None else None,
                      headers=headers)
    with urlopen(request, timeout=60) as response:
        body = response.read()
        return json.loads(body) if body else None


def http_error_message(error):
    body = error.read().decode(errors="replace").strip()
    error.close()
    try:
        message = json.loads(body).get("message", body)
    except (AttributeError, json.JSONDecodeError):
        message = body
    detail = f": {message}" if message else ""
    return f"HTTP {error.code} {error.reason}{detail}"


def prepare(requested, dry_run, automated=False):
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
    branch = f"release/v{version}"
    run("git", "switch", "-c", branch)
    (ROOT / "Cargo.toml").write_text(manifest)
    (ROOT / "Cargo.lock").write_text(lock)
    validate()
    run("git", "add", "Cargo.toml", "Cargo.lock")
    run("git", "commit", "-m", f"chore: prepare release v{version}")
    run("git", "push", "-u", "origin", branch)
    if automated:
        repository = os.environ["RELEASE_REPOSITORY"]
        return api(f"/repos/{repository}/pulls", {
            "title": f"Release v{version}", "base": "main", "head": branch,
            "body": f"Prepare Shoal {version}. Validated formatting, Clippy, tests, and release build. "
                    "The Release workflow will merge this version change and publish it automatically.",
        })
    with tempfile.NamedTemporaryFile(mode="w", suffix=".md") as body:
        body.write(f"Prepare Shoal {version}. Both Cargo versions are updated.\n\n"
                   "Validated formatting, Clippy, tests, and the release build.\n\n"
                   f"After merging, run `python3 scripts/release.py publish {version}`. "
                   "Alternatively use Actions → release for the fully automated process.\n")
        body.flush()
        run("fj", "pr", "create", f"Release v{version}", "--base", "main",
            "--head", branch, "--body-file", body.name)


def publish(version, dry_run, merged_commit=None):
    parts(version)
    if git("status", "--porcelain"):
        raise ValueError("commit or stash changes before publishing a release")
    run("git", "fetch", "origin", "+refs/heads/main:refs/remotes/origin/main", "--tags")
    sha = merged_commit or git("rev-parse", "origin/main")
    git("merge-base", "--is-ancestor", sha, "origin/main")
    # Validate what will actually be tagged, not the caller's old checkout.
    if git("rev-parse", "HEAD") != sha:
        raise ValueError("check out the release commit before publishing")
    if versions(git("show", f"{sha}:Cargo.toml"), git("show", f"{sha}:Cargo.lock")) != version:
        raise ValueError("requested version is not merged into main")
    tag = version_tag(version)
    existing = git("tag", "--list", tag)
    if existing and git("rev-parse", f"refs/tags/{tag}^{{commit}}") != sha:
        raise ValueError("release tag already points elsewhere; it will not be moved")
    if dry_run:
        print(f"Would publish {tag} at {sha}")
        return
    validate()
    if not existing:
        run("git", "tag", "-a", tag, sha, "-m", f"Shoal {version}")
    run("git", "push", "origin", f"refs/tags/{tag}")
    create_release(version)


def create_release(version):
    repository = os.environ.get("RELEASE_REPOSITORY", "HN05/shoal")
    server = os.environ.get("RELEASE_API_URL", "https://git.henriknordvik.com/api/v1")
    url = server.rstrip("/").removesuffix("/api/v1") + "/" + repository
    tag = version_tag(version)
    try:
        existing = api(f"/repos/{repository}/releases/tags/{tag}", authenticated=False)
    except HTTPError as error:
        if error.code != 404:
            raise
        error.close()
    else:
        if existing.get("tag_name") != tag:
            raise ValueError("existing release does not match the requested tag")
        print(f"Forgejo release {tag} already exists")
        return
    body = release_notes.generate(
        tag, git,
        lambda path: api(f"/repos/{repository}{path}", authenticated=False), url, repository)
    # CI calls repository endpoints directly; no user-profile/login API is needed.
    if os.environ.get("RELEASE_AUTOMATION_TOKEN"):
        api(f"/repos/{repository}/releases", {
            "name": f"Shoal {version}", "tag_name": f"v{version}",
            "body": body, "draft": False, "prerelease": False,
        })
    else:
        run("fj", "release", "create", f"Shoal {version}", "--tag", f"v{version}",
            "--body", body)


def merged_release(pr, repository, number):
    """Accept only the server-confirmed merge of this repository's release PR."""
    if (pr.get("merged") is not True or pr.get("number") != number or number < 1
            or pr.get("base", {}).get("ref") != "main"
            or pr.get("base", {}).get("repo", {}).get("full_name") != repository
            or pr.get("head", {}).get("repo", {}).get("full_name") != repository):
        raise ValueError("expected a merged release PR from this repository into main")
    branch = pull_branch(pr, repository, number)
    if not branch.startswith("release/v"):
        raise ValueError("expected a release/vMAJOR.MINOR.PATCH branch")
    version = tag_version(branch.removeprefix("release/"))
    sha = commit_id(pr.get("merge_commit_sha", ""))
    return version, sha


def publish_merged(pr, repository, number):
    version, sha = merged_release(pr, repository, number)
    if git("status", "--porcelain"):
        raise ValueError("publish-pr requires a clean checkout")
    run("git", "fetch", "origin", "+refs/heads/main:refs/remotes/origin/main", "--tags")
    git("merge-base", "--is-ancestor", sha, "origin/main")
    if versions(git("show", f"{sha}:Cargo.toml"), git("show", f"{sha}:Cargo.lock")) != version:
        raise ValueError("merged PR version does not match its release branch")
    run("git", "checkout", "--detach", sha)
    publish(version, False, merged_commit=sha)


def resume(version):
    if git("status", "--porcelain"):
        raise ValueError("resume requires a clean checkout")
    tag = version_tag(version)
    run("git", "fetch", "origin", "+refs/heads/main:refs/remotes/origin/main", "--tags")
    if not git("tag", "--list", tag):
        raise ValueError("the current version can only resume from its existing release tag")
    sha = git("rev-parse", f"refs/tags/{tag}^{{commit}}")
    git("merge-base", "--is-ancestor", sha, "origin/main")
    if versions(git("show", f"{sha}:Cargo.toml"), git("show", f"{sha}:Cargo.lock")) != version:
        raise ValueError("release tag does not contain the requested version")
    run("git", "checkout", "--detach", sha)
    publish(version, False, merged_commit=sha)


def release_all(requested):
    current = versions((ROOT / "Cargo.toml").read_text(), (ROOT / "Cargo.lock").read_text())
    if requested and parts(requested) == parts(current):
        resume(requested)
        return
    repository = os.environ["RELEASE_REPOSITORY"]
    pr = prepare(requested, False, automated=True)
    number = pr["number"]
    # Pin the merge to the exact validated release commit and respect branch
    # protection. Never use force_merge or merge an unexpected branch update.
    merge = {"Do": "rebase", "head_commit_id": git("rev-parse", "HEAD"),
             "delete_branch_after_merge": True}
    # Forgejo may still be computing mergeability just after PR creation.
    # Retry only its temporary/not-allowed response; protection remains intact.
    for attempt in range(30):
        try:
            api(f"/repos/{repository}/pulls/{number}/merge", merge)
            break
        except HTTPError as error:
            if error.code != 405 or attempt == 29:
                raise
            time.sleep(2)
    merged = api(f"/repos/{repository}/pulls/{number}")
    publish_merged(merged, repository, number)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    prep = commands.add_parser("prepare", help="bump versions, validate, and open a release PR")
    prep.add_argument("version", nargs="?")
    pub = commands.add_parser("publish", help="validate merged main, tag, and create a Forgejo release")
    pub.add_argument("version")
    for command in (prep, pub):
        command.add_argument("--dry-run", action="store_true")
    automated = commands.add_parser("run", help="CI: prepare, merge, validate, and publish a release")
    automated.add_argument("version", nargs="?")
    args = parser.parse_args()
    try:
        if args.command == "run":
            release_all(args.version)
        else:
            (prepare if args.command == "prepare" else publish)(args.version, args.dry_run)
    except HTTPError as error:
        parser.exit(1, f"Release failed: {http_error_message(error)}\n")
    except (ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Release failed: {error}\n")


if __name__ == "__main__":
    main()
