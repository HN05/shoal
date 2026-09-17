#!/usr/bin/env python3
"""Update only Shoal's tap entry from an existing release tag (Python 3.11+)."""
import argparse
from pathlib import Path
import re
import subprocess
import tomllib
from urllib.parse import urlparse


def release_version(value):
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", value):
        raise ValueError("expected a stable major.minor.patch version")
    return tuple(map(int, value.split(".")))


def update_formula(text, version, revision, source_url="https://github.com/HN05/shoal.git"):
    parsed = urlparse(source_url)
    if (parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password
            or parsed.path != "/HN05/shoal.git" or parsed.query or parsed.fragment):
        raise ValueError("expected an HTTPS Shoal repository URL")
    new_version = release_version(version)
    if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", revision):
        raise ValueError("expected a full Git commit ID")
    versions = re.findall(r'^  version "([^"]+)"$', text, re.M)
    if len(versions) != 1:
        raise ValueError("expected exactly one formula version")
    if new_version < release_version(versions[0]):
        raise ValueError("refusing to downgrade the release channel")
    pattern = (rf'^  url "{re.escape(source_url)}", '
               r'tag: "v[^"\n]+"(?:, revision: "([0-9a-f]+)")?$')
    sources = list(re.finditer(pattern, text, re.M))
    if len(sources) != 1:
        raise ValueError("expected exactly one Shoal release source")
    old_revision = sources[0].group(1)
    if version == versions[0] and old_revision and old_revision != revision:
        raise ValueError("refusing to move an already pinned release tag")
    text = re.sub(pattern,
                  f'  url "{source_url}", '
                  f'tag: "v{version}", revision: "{revision}"', text, flags=re.M)
    return re.sub(r'^  version "[^"]+"$', f'  version "{version}"', text, flags=re.M)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("tap", type=Path)
    parser.add_argument("--source-url", required=True,
                        help="Expected HTTPS source repository URL for this tap")
    args = parser.parse_args()
    if not args.tag.startswith("v"):
        parser.error("release tags must start with v")
    version = args.tag[1:]
    release_version(version)
    source = Path(__file__).resolve().parent.parent

    def git(*arguments):
        return subprocess.check_output(["git", "-C", str(source), *arguments], text=True).strip()

    revision = git("rev-parse", "--verify", f"refs/tags/{args.tag}^{{commit}}")
    package = tomllib.loads(git("show", f"{revision}:Cargo.toml"))["package"]
    if package["name"] != "shoal" or package["version"] != version:
        raise ValueError("release tag must match Shoal's Cargo package version")
    git("cat-file", "-e", f"{revision}:scripts/install-homebrew.sh")
    if args.source_url == "https://github.com/HN05/shoal.git":
        # Do not publish a formula before the source mirror has the exact tag.
        refs = git("ls-remote", "https://github.com/HN05/shoal.git",
                   f"refs/tags/{args.tag}", f"refs/tags/{args.tag}^{{}}")
        targets = dict(line.split()[::-1] for line in refs.splitlines())
        mirrored = targets.get(f"refs/tags/{args.tag}^{{}}", targets.get(f"refs/tags/{args.tag}"))
        if mirrored != revision:
            raise ValueError("GitHub source tag is missing or differs; wait for the Shoal mirror and retry")
    formula = args.tap / "Formula/shoal.rb"
    original = formula.read_text()
    updated = update_formula(original, version, revision, args.source_url)
    if updated != original:
        formula.write_text(updated)
    print(f"Shoal {args.tag}: {revision}")


if __name__ == "__main__":
    main()
