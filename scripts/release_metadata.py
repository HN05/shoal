"""Pure validation of stable release versions and immutable Git tag identity."""
import re

STABLE_VERSION = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
STABLE_TAG = re.compile("v" + STABLE_VERSION)


def version_parts(version):
    if not re.fullmatch(STABLE_VERSION, version):
        raise ValueError("expected a stable MAJOR.MINOR.PATCH version without v")
    return tuple(map(int, version.split(".")))


def tag_version(tag):
    if not STABLE_TAG.fullmatch(tag):
        raise ValueError("expected a stable vMAJOR.MINOR.PATCH tag")
    return tag[1:]


def version_tag(version):
    version_parts(version)
    return f"v{version}"


def commit_id(revision):
    if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", revision):
        raise ValueError("expected a full Git commit ID")
    return revision


def verified_tag_commit(refs, tag, revision):
    """Verify ls-remote output against a commit; return None for an absent tag.

    Prefer the peeled target of an annotated tag over its tag object. Callers
    own remote access and decide whether and how to retry a missing tag.
    """
    tag_version(tag)
    commit_id(revision)
    targets = dict(line.split()[::-1] for line in refs.splitlines())
    ref = f"refs/tags/{tag}"
    found = targets.get(ref + "^{}", targets.get(ref))
    if found is not None:
        commit_id(found)
        if found != revision:
            raise ValueError(f"tag {tag} points at {found}, not the released {revision}")
    return found


def pull_branch(pr, repository, number):
    """Recover a deleted branch label only for this repository's exact PR."""
    head = pr.get("head", {})
    branch = head.get("ref", "")
    if (number > 0 and pr.get("number") == number
            and branch == f"refs/pull/{number}/head"
            and pr.get("base", {}).get("repo", {}).get("full_name") == repository
            and head.get("repo", {}).get("full_name") == repository):
        return head.get("label", "")
    return branch
