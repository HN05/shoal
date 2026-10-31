#!/usr/bin/env python3
"""Create or update the repository's label scheme with your Shoal login.

Idempotent: an existing label of the same name is edited in place. Needs write
access to issues, so run it as the owner from the registered checkout, not
inside a workspace, whose agent account can only read:

    python3 .forgejo/scripts/labels.py

`area/` allows several labels per issue; `type/` and `complexity/` are exclusive.
Reviewer request labels are combinable; keep .review/review.md in step when this changes.
"""
import json
import subprocess

LABELS = [  # name, colour, exclusive, description
    ("area/cli",          "1d4ed8", False, "Command-line interface, execution wrapper, terminal prompts and completions"),
    ("area/daemon",       "0f766e", False, "Daemon, persisted state, lifecycle, reconciliation and service setup"),
    ("area/workspaces",   "7c3aed", False, "Worktrees, branches, merge, diff, pull and automatic cleanup"),
    ("area/resources",    "c2410c", False, "Ports, simulator leases and semaphore permits"),
    ("area/agents",       "0e7490", False, "Agent skills, agent prompts and forge-issue workspace creation"),
    ("area/docs",         "6b7280", False, "README, reference, design and contributor documentation"),
    ("area/infra",        "b45309", False, "Developer tooling, CI, releases, Homebrew taps and hosting"),
    ("blocked",           "000000", False, "Cannot proceed until something outside the issue happens"),
    ("needs-plan",        "fbbf24", False, "Needs a plan settled with the owner before anyone implements it"),
    ("planned",           "16a34a", False, "The issue body holds a settled plan; an agent can implement it without input"),
    ("tracking",          "6e7781", False, "A standing issue a workflow rewrites and nobody closes"),
    ("complexity/tiny",   "fbcfe8", True,  "One file or a few call sites; the diff is obvious from the issue body"),
    ("complexity/small",  "f472b6", True,  "One module; the shape is settled and no schema or design text moves"),
    ("complexity/medium", "db2777", True,  "Crosses CLI and daemon, or changes persisted state, or leaves a decision on the way"),
    ("complexity/large",  "831843", True,  "A vertical slice or a crate-wide sweep; schema, design or agent contract moves"),
    ("type/bug",          "b91c1c", True,  "Something built does not behave as specified"),
    ("type/feature",      "15803d", True,  "New capability from the design"),
    ("type/chore",        "64748b", True,  "Maintenance, cleanup, dependency and tooling work"),
    ("type/test",         "0369a1", True,  "Tests, regression coverage and test fixtures"),
    ("review/default",    "0075ca", False, "Request the repository default reviewer; consumed at run start"),
    ("review/claude",     "d876e3", False, "Request Claude as primary reviewer; consumed at run start"),
    ("review/codex",      "0e8a16", False, "Request Codex as primary reviewer; consumed at run start"),
    ("review/none",       "e4e669", True,  "Suppress automated review until this label is removed"),
]


def api(path, *args, data=None):
    """Send a request to the repository's forge API through Shoal."""
    if data is not None:
        args = (*args, "--input", "-")
    output = subprocess.run(["shoal", "forge", "api", path, "--json", *args], check=True,
                            text=True, stdout=subprocess.PIPE,
                            input=None if data is None else json.dumps(data)).stdout
    return json.loads(output)


def existing():
    """The repository's label IDs by name."""
    ids, page = {}, 1
    while labels := api(f"repos/{{owner}}/{{repo}}/labels?limit=50&page={page}"):
        ids.update((label["name"], label["id"]) for label in labels)
        page += 1
    return ids


def main():
    ids = existing()
    for name, colour, exclusive, description in LABELS:
        label = {"name": name, "color": f"#{colour}", "exclusive": exclusive,
                 "description": description}
        if name in ids:
            api(f"repos/{{owner}}/{{repo}}/labels/{ids[name]}", "-X", "PATCH", data=label)
            print(f"updated {name}")
        else:
            api("repos/{owner}/{repo}/labels", data=label)
            print(f"created {name}")


if __name__ == "__main__":
    main()
