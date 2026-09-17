#!/usr/bin/env sh
# Create or update the repository's label scheme with the current `fj` login.
# Idempotent: an existing label of the same name is edited in place. Needs
# write access to issues, so run it as the owner, not as an agent account.
#
#   .forgejo/scripts/labels.sh            from a checkout whose origin is the repo
#
# `area/` allows several labels per issue; `type/`, `complexity/` and `review/`
# are exclusive scopes. Keep .review/review.md in step when this changes.
set -eu

label() { # name colour exclusive(true/false) description
  exclusive=''
  [ "$3" = true ] && exclusive=-e
  # shellcheck disable=SC2086
  if fj repo labels create "$1" "$2" -d "$4" $exclusive >/dev/null 2>&1; then
    echo "created $1"
  else
    # Already exists (or the create failed for another reason, which edit reports).
    fj repo labels edit "$1" -c "$2" -d "$4" -e "$3" >/dev/null
    echo "updated $1"
  fi
}

label area/cli        1d4ed8 false "Command-line interface, execution wrapper, terminal prompts and completions"
label area/daemon     0f766e false "Daemon, persisted state, lifecycle, reconciliation and service setup"
label area/workspaces 7c3aed false "Worktrees, branches, merge, diff, pull and automatic cleanup"
label area/resources  c2410c false "Ports, simulator leases and semaphore permits"
label area/agents     0e7490 false "SKILL.md, agent prompts and forge-issue workspace creation"
label area/docs       6b7280 false "README, reference, design and contributor documentation"
label area/infra      b45309 false "Developer tooling, CI, releases, Homebrew taps and hosting"
label blocked         000000 false "Cannot proceed until something outside the issue happens"
label needs-decision  fbbf24 false "Waiting on the owner to choose between options"
label tracking        6e7781 false "A standing issue a workflow rewrites and nobody closes"
label complexity/tiny   fbcfe8 true "One file or a few call sites; the diff is obvious from the issue body"
label complexity/small  f472b6 true "One module; the shape is settled and no schema or design text moves"
label complexity/medium db2777 true "Crosses CLI and daemon, or changes persisted state, or leaves a decision on the way"
label complexity/large  831843 true "A vertical slice or a crate-wide sweep; schema, design or agent contract moves"
label type/bug        b91c1c true "Something built does not behave as specified"
label type/feature    15803d true "New capability from the design"
label type/chore      64748b true "Maintenance, cleanup, dependency and tooling work"
label type/test       0369a1 true "Tests, regression coverage and test fixtures"
label review/default  0075ca true "Request the repository default reviewer; consumed at run start"
label review/claude   d876e3 true "Request Claude as primary reviewer; consumed at run start"
label review/codex    0e8a16 true "Request Codex as primary reviewer; consumed at run start"
label review/none     e4e669 true "Suppress automated review until this label is removed"
