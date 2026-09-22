#!/usr/bin/env python3
"""Update only Shoal's tap entry from an existing release tag (Python 3.11+)."""
import argparse
from pathlib import Path
import re
import subprocess
import tomllib
from urllib.parse import urlparse
from urllib.request import urlopen


def release_version(value):
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", value):
        raise ValueError("expected a stable major.minor.patch version")
    return tuple(map(int, value.split(".")))


PLATFORMS = ("macos-arm64", "macos-x86_64", "linux-arm64", "linux-x86_64")


def release_checksums(manifest, version):
    checksums = {}
    for line in manifest.splitlines():
        match = re.fullmatch(r"([0-9a-f]{64}) [ *](\S+)", line)
        if not match or match[2] in checksums:
            raise ValueError("invalid or duplicate release checksum")
        checksums[match[2]] = match[1]
    expected = {f"shoal-v{version}-{platform}.tar.gz" for platform in PLATFORMS}
    if checksums.keys() != expected:
        raise ValueError("expected checksums for every release platform")
    return checksums


def update_formula(text, version, revision, manifest,
                   source_url="https://github.com/HN05/shoal.git"):
    parsed = urlparse(source_url)
    if (parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password
            or parsed.path != "/HN05/shoal.git" or parsed.query or parsed.fragment):
        raise ValueError("expected an HTTPS Shoal repository URL")
    new_version = release_version(version)
    if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", revision):
        raise ValueError("expected a full Git commit ID")
    versions = re.findall(r'^  version "([^"\n]+)"$', text, re.M)
    if len(versions) != 1:
        raise ValueError("expected exactly one formula version")
    if new_version < release_version(versions[0]):
        raise ValueError("refusing to downgrade the release channel")
    # Accept the original source formula once, then only our generated formula.
    legacy = re.search(rf'^  url "{re.escape(source_url)}", tag: "v{re.escape(versions[0])}"'
                       r'(?:, revision: "([0-9a-f]+)")?$', text, re.M)
    pinned = re.search(r'^  # Release commit: ([0-9a-f]+)$', text, re.M)
    if not legacy and not pinned:
        raise ValueError("expected a Shoal release source or pinned release commit")
    old_revision = (legacy or pinned)[1]
    if version == versions[0] and old_revision and old_revision != revision:
        raise ValueError("refusing to move an already pinned release tag")
    repository = source_url.removesuffix(".git")
    download = f"{repository}/releases/download/v{version}"
    checksums = release_checksums(manifest, version)
    blocks = []
    for platform_os in ("macos", "linux"):
        blocks.append(f"  on_{platform_os} do")
        for arch, platform_arch in (("arm", "arm64"), ("intel", "x86_64")):
            name = f"shoal-v{version}-{platform_os}-{platform_arch}.tar.gz"
            blocks.extend([f"    on_{arch} do", f'      url "{download}/{name}"',
                           f'      sha256 "{checksums[name]}"', "    end"])
        blocks.append("  end")
    template = Path(__file__).with_name("shoal.rb.template").read_text()
    # A generated formula must match its recorded version and sources before replacement.
    if not legacy:
        assets = re.findall(r'^      url "([^"\n]+)"\n      sha256 "([0-9a-f]{64})"$', text, re.M)
        old_manifest = "\n".join(f"{checksum}  {url.rsplit('/', 1)[-1]}"
                                 for url, checksum in assets)
        old_checksums = release_checksums(old_manifest, versions[0])
        expected_urls = {f'{repository}/releases/download/v{versions[0]}/{name}'
                         for name in old_checksums}
        if {url for url, _ in assets} != expected_urls:
            raise ValueError("unexpected release asset URLs")
        if version == versions[0] and checksums != old_checksums:
            raise ValueError("refusing to change already pinned release checksums")
    for key, value in (("REPOSITORY", repository), ("REVISION", revision),
                       ("VERSION", version), ("RELEASE_SOURCES", "\n".join(blocks))):
        template = template.replace(f"@{key}@", value)
    return template


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
    download = args.source_url.removesuffix(".git") + f"/releases/download/{args.tag}"
    with urlopen(f"{download}/SHA256SUMS", timeout=60) as response:
        manifest = response.read().decode("utf-8")
    updated = update_formula(original, version, revision, manifest, args.source_url)
    if updated != original:
        formula.write_text(updated)
    print(f"Shoal {args.tag}: {revision}")


if __name__ == "__main__":
    main()
