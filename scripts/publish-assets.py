#!/usr/bin/env python3
"""Attach release binaries to the Forgejo release and mirror it to GitHub.

`forgejo <tag> <files...>` uploads to the existing Forgejo release for the tag.
`github <tag> <files...>` waits for the push mirror to carry the tag, creates the
GitHub release when missing, and uploads the files. Both skip files whose name
is already attached, so a rerun after a partial failure completes the release.
"""
import argparse
import json
import mimetypes
import os
from pathlib import Path
import re
import subprocess
import time
import uuid
from urllib.error import HTTPError
from urllib.request import Request, urlopen

GITHUB_API = "https://api.github.com"
GITHUB_UPLOADS = "https://uploads.github.com"


def version(tag):
    if not re.fullmatch(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", tag):
        raise ValueError("expected a stable vMAJOR.MINOR.PATCH tag")
    return tag[1:]


def request(url, token, scheme, data=None, content_type="application/json"):
    body = json.dumps(data).encode() if isinstance(data, dict) else data
    headers = {"Authorization": f"{scheme} {token}", "Accept": "application/json"}
    if body is not None:
        headers["Content-Type"] = content_type
    with urlopen(Request(url, data=body, headers=headers), timeout=300) as response:
        payload = response.read()
        return json.loads(payload) if payload else None


def multipart(path):
    """One `attachment` file part, as Forgejo's asset endpoint expects."""
    boundary = uuid.uuid4().hex
    head = (f"--{boundary}\r\nContent-Disposition: form-data; name=\"attachment\"; "
            f"filename=\"{path.name}\"\r\nContent-Type: application/octet-stream\r\n\r\n").encode()
    return head + path.read_bytes() + f"\r\n--{boundary}--\r\n".encode(), \
        f"multipart/form-data; boundary={boundary}"


def missing(files, existing):
    names = {asset["name"] for asset in existing}
    for path in files:
        if path.name in names:
            print(f"{path.name}: already attached")
        else:
            yield path


def forgejo(tag, files):
    base = os.environ["RELEASE_API_URL"].rstrip("/") + "/repos/" + os.environ["RELEASE_REPOSITORY"]
    token = os.environ["RELEASE_AUTOMATION_TOKEN"]
    release = request(f"{base}/releases/tags/{tag}", token, "token")
    for path in missing(files, release["assets"]):
        body, content_type = multipart(path)
        request(f"{base}/releases/{release['id']}/assets?name={path.name}", token, "token",
                body, content_type)
        print(f"{path.name}: attached to Forgejo release {tag}")


def mirrored(repository, tag, revision, attempts=30, delay=20):
    """Wait until GitHub's copy of the tag points at the released commit."""
    for attempt in range(attempts):
        refs = subprocess.check_output(
            ["git", "ls-remote", f"https://github.com/{repository}.git",
             f"refs/tags/{tag}", f"refs/tags/{tag}^{{}}"], text=True)
        targets = dict(line.split()[::-1] for line in refs.splitlines())
        found = targets.get(f"refs/tags/{tag}^{{}}", targets.get(f"refs/tags/{tag}"))
        if found == revision:
            return
        if found is not None:
            raise ValueError(f"GitHub tag {tag} points at {found}, not the released {revision}")
        if attempt + 1 < attempts:
            print(f"waiting for the GitHub mirror to receive {tag}")
            time.sleep(delay)
    raise ValueError(f"GitHub mirror never received {tag}; check the push mirror and rerun")


def github(tag, files, revision):
    repository = os.environ["RELEASE_REPOSITORY"]
    token = os.environ["GITHUB_RELEASE_TOKEN"]
    mirrored(repository, tag, revision)
    base = f"{GITHUB_API}/repos/{repository}/releases"
    try:
        release = request(f"{base}/tags/{tag}", token, "Bearer")
    except HTTPError as error:
        if error.code != 404:
            raise
        release = request(base, token, "Bearer", {
            "tag_name": tag, "name": f"Shoal {version(tag)}", "draft": False, "prerelease": False,
            "body": f"Shoal {version(tag)}.\n\nInstall or upgrade through the "
                    "[HN05 Homebrew tap](https://github.com/HN05/homebrew-tap), or download a "
                    "binary below. Development happens on "
                    f"[git.henriknordvik.com](https://git.henriknordvik.com/{repository}).",
        })
        print(f"created GitHub release {tag}")
    if release.get("draft") or release["tag_name"] != tag:
        raise ValueError(f"unexpected GitHub release for {tag}")
    for path in missing(files, release["assets"]):
        content_type = mimetypes.guess_type(path.name)[0] or "application/octet-stream"
        request(f"{GITHUB_UPLOADS}/repos/{repository}/releases/{release['id']}/assets?name={path.name}",
                token, "Bearer", path.read_bytes(), content_type)
        print(f"{path.name}: attached to GitHub release {tag}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("forge", choices=["forgejo", "github"])
    parser.add_argument("tag")
    parser.add_argument("files", nargs="+", type=Path)
    parser.add_argument("--revision", help="released commit the GitHub tag must match")
    args = parser.parse_args()
    try:
        version(args.tag)
        for path in args.files:
            if not path.is_file():
                raise ValueError(f"{path} is not a file")
        if args.forge == "forgejo":
            forgejo(args.tag, args.files)
        else:
            if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", args.revision or ""):
                raise ValueError("github needs --revision with the released commit ID")
            github(args.tag, args.files, args.revision)
    except (ValueError, KeyError, subprocess.CalledProcessError, HTTPError) as error:
        parser.exit(1, f"Publishing assets failed: {error}\n")


if __name__ == "__main__":
    main()
