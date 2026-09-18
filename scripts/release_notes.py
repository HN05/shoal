"""Release changelogs from published tags and merged Forgejo pull requests."""
import re
from urllib.error import HTTPError

STABLE_TAG = re.compile(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")


def generate(tag, git, get, url):
    def pages(path):
        page = 1
        while True:
            separator = "&" if "?" in path else "?"
            batch = get(f"{path}{separator}limit=50&page={page}")
            if not isinstance(batch, list):
                raise ValueError(f"expected an array from {path}")
            if not batch:
                return
            yield from batch
            page += 1

    def version(value):
        return tuple(map(int, STABLE_TAG.fullmatch(value).groups()))

    target = git("rev-parse", f"{tag}^{{commit}}")
    history = set(git("rev-list", target).splitlines())
    previous, distance = None, float("inf")
    for release in pages("/releases"):
        candidate = release["tag_name"]
        if (release.get("draft") or release.get("prerelease")
                or not STABLE_TAG.fullmatch(candidate) or version(candidate) >= version(tag)):
            continue
        sha = git("rev-parse", f"{candidate}^{{commit}}")
        if sha not in history:
            continue
        count = int(git("rev-list", "--count", f"{sha}..{target}"))
        if count < distance:
            previous, distance = candidate, count
    commits = set(git("rev-list", f"{previous}..{target}").splitlines()) if previous else history
    pulls = sorted((pr for pr in pages("/pulls?state=closed")
                    if pr.get("merged") and pr["base"]["ref"] == "main"
                    and pr.get("merge_commit_sha") in commits), key=lambda pr: pr["number"])
    lines = [f"Shoal {tag[1:]}.", "", "Install or upgrade through the "
             "[HN05 Homebrew tap](https://github.com/HN05/homebrew-tap), or download a "
             "prebuilt Linux or macOS binary below.", "",
             f"## Changes since {previous}" if previous else "## Changes in this first release", ""]
    issues = {}
    for pr in pulls:
        text = re.sub(r"```[\s\S]*?```", "", f"{pr['title']}\n{pr.get('body') or ''}")
        numbers = set(re.findall(r"(?:^|[\s(,:])#([1-9][0-9]*)\b", text))
        numbers.update(re.findall(re.escape(url) + r"/issues/([1-9][0-9]*)\b", text))
        links = []
        for number in sorted(map(int, numbers)):
            if number not in issues:
                try:
                    issue = get(f"/issues/{number}")
                except HTTPError as error:
                    if error.code != 404:
                        raise
                    error.close()
                    issue = None
                issues[number] = issue is not None and "pull_request" not in issue
            if issues[number]:
                links.append(f"[#{number}]({url}/issues/{number})")
        title = re.sub(r"[\r\n]+", " ", pr["title"])
        title = re.sub(r"[\\`*_\[\]<>]", lambda match: "\\" + match[0], title)
        line = f"- {title} ([#{pr['number']}]({url}/pulls/{pr['number']}))"
        lines.append(line + (f" — Issues: {', '.join(links)}" if links else ""))
    if not pulls:
        lines.append("No merged pull requests in this release range.")
    if previous:
        lines.extend(["", f"[Full changes]({url}/compare/{previous}...{tag})"])
    return "\n".join(lines)
