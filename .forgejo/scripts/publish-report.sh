#!/usr/bin/env sh
# Publish a report body into the evergreen dependency-report issue.
#
#   publish-report.sh <body-file>
#
# The issue is found by the HTML-comment marker on the body's first line rather
# than a hardcoded number, so it survives a retitle, relabel or transfer; the
# oldest open issue carrying the marker wins, so a stray duplicate cannot take
# over. The body is replaced in full and the issue is never closed.
#
# Environment:
#   FORGE_API      API root, e.g. https://git.henriknordvik.com/api/v1
#   FORGE_REPO     owner/repo
#   FORGE_TOKEN    token with write access to that repo (the job token in CI)
#   ISSUE_TITLE    title used only when the issue has to be created
#   ISSUE_LABELS   comma-separated label names, likewise only used at creation
#
# Any API failure is fatal: a report nobody can see is worse than a red run.
set -eu

[ $# -eq 1 ] || {
  echo "usage: $0 <body-file>" >&2
  exit 2
}
body=$1
[ -s "$body" ] || {
  echo "publish-report: $body is empty" >&2
  exit 2
}

: "${FORGE_API:?}" "${FORGE_REPO:?}" "${FORGE_TOKEN:?}"
ISSUE_TITLE=${ISSUE_TITLE:-Dependency report}
ISSUE_LABELS=${ISSUE_LABELS:-}

marker=$(head -n 1 "$body")
case "$marker" in
  '<!--'*'-->') ;;
  *)
    echo "publish-report: the body's first line is not the marker comment: $marker" >&2
    exit 2
    ;;
esac

api() { # method path [json-body-file]
  method=$1
  path=$2
  if [ $# -ge 3 ]; then
    curl -sS --fail-with-body -X "$method" \
      -H "Authorization: token $FORGE_TOKEN" \
      -H 'Content-Type: application/json' \
      --data-binary "@$3" \
      "$FORGE_API/$path"
  else
    curl -sS --fail-with-body -X "$method" \
      -H "Authorization: token $FORGE_TOKEN" \
      "$FORGE_API/$path"
  fi
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# sort=oldest puts the evergreen issue on the first page, but the search still
# pages to the end before concluding there is none: concluding that wrongly
# opens a second issue. The page cap fails instead of duplicating.
number=''
page=1
while [ "$page" -le 20 ]; do
  api GET "repos/$FORGE_REPO/issues?state=open&type=issues&sort=oldest&limit=50&page=$page" >"$work/issues.json"
  [ "$(jq 'length' "$work/issues.json")" -gt 0 ] || break
  number=$(jq -r --arg m "$marker" \
    '[.[] | select((.body // "") | contains($m)) | .number] | min // empty' \
    "$work/issues.json")
  [ -z "$number" ] || break
  page=$((page + 1))
done
if [ -z "$number" ] && [ "$page" -gt 20 ]; then
  echo "publish-report: gave up after 20 pages of open issues without finding the marker; refusing to open a second report" >&2
  exit 1
fi

if [ -n "$number" ]; then
  jq -n --rawfile body "$body" '{body: $body}' >"$work/patch.json"
  api PATCH "repos/$FORGE_REPO/issues/$number" "$work/patch.json" >"$work/result.json"
  echo "updated $(jq -r '.html_url' "$work/result.json")"
  exit 0
fi

# First run, or the issue was deleted. Forgejo takes label ids, not names.
labels='[]'
if [ -n "$ISSUE_LABELS" ]; then
  api GET "repos/$FORGE_REPO/labels?limit=100" >"$work/labels.json"
  labels=$(jq -c --arg want "$ISSUE_LABELS" \
    '($want | split(",")) as $names | [.[] | select(.name as $n | $names | index($n)) | .id]' \
    "$work/labels.json")
  found=$(printf '%s' "$labels" | jq 'length')
  wanted=$(printf '%s' "$ISSUE_LABELS" | tr ',' '\n' | grep -c .)
  # A missing label is worth saying out loud but not worth losing the report over.
  [ "$found" -eq "$wanted" ] || echo "publish-report: only $found of $wanted labels exist in $FORGE_REPO" >&2
fi

jq -n --arg title "$ISSUE_TITLE" --rawfile body "$body" --argjson labels "$labels" \
  '{title: $title, body: $body, labels: $labels}' >"$work/create.json"
api POST "repos/$FORGE_REPO/issues" "$work/create.json" >"$work/result.json"
echo "created $(jq -r '.html_url' "$work/result.json")"
