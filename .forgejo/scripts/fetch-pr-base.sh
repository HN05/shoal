#!/usr/bin/env sh
# Fetches a PR's base commit into a Docker job's shallow checkout of the PR,
# deepening both histories until they meet, as scripts/affected_packages.py
# and diffs against the base need. A base it cannot fetch is left out, and
# the job's steps then run everything. Does nothing outside PRs.
#
# Reads PR_BASE, empty outside PRs, SHA, the commit checked out, and TOKEN.
set -eu
[ -n "$PR_BASE" ] || exit 0
auth="AUTHORIZATION: basic $(printf 'x-access-token:%s' "$TOKEN" | base64 | tr -d '\n')"
git -c http.extraheader="$auth" fetch -q --depth 1 origin "$PR_BASE" ||
  echo "could not fetch the PR's base $PR_BASE"
for depth in 50 500; do
  git merge-base "$PR_BASE" HEAD > /dev/null 2>&1 && break
  git -c http.extraheader="$auth" fetch -q --depth "$depth" origin "$SHA" "$PR_BASE" ||
    break
done
