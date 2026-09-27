#!/usr/bin/env sh
# Print the Markdown body of the evergreen dependency report on stdout, from
# cargo audit and cargo outdated. deps.yml runs it weekly and hands the body to
# publish-report.sh; it also runs from a checkout.
#
# Findings are never an error; advisories are the product. The exit status says
# only whether the report can be trusted:
#   0  complete, or a tool is not installed (a banner in the body says which)
#   1  an installed tool failed or printed something this cannot read. The body
#      says so too, so publish it first and fail the job afterwards.
#
# Optional: RUN_URL and SOURCE are linked in the footer.
set -eu

MARKER='<!-- shoal:dependency-report -->'
ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

missing=''     # tools that are not installed: degrade, say so, exit 0
broken=''      # tools that ran and failed: say so and exit 1
broken_errs='' # their stderr files, quoted in the banner
advisories=0

note_missing() { missing="${missing:+$missing, }$1"; }
note_broken() { # tool-name stderr-file
  broken="${broken:+$broken, }$1"
  broken_errs="${broken_errs:+$broken_errs }$2"
}

# A table, or a plain sentence when empty: a quiet week should read at a glance.
table() { # header-row separator-row rows-file empty-text
  if [ -s "$3" ]; then
    printf '%s\n%s\n' "$1" "$2"
    cat "$3"
  else
    printf '%s\n' "$4"
  fi
  printf '\n'
}

# A failing tool's first stderr lines, so the issue says what broke without
# anyone opening the run log.
stderr_excerpt() {
  if [ -s "$1" ]; then
    printf '>\n> ```\n'
    sed -n '1,5p' "$1" | sed 's/^/> /'
    printf '> ```\n'
  fi
}

# ---------------------------------------------------------------------- audit

adv=$WORK/adv.md
warn=$WORK/warn.md
: >"$adv"
: >"$warn"
audit_note=''
if command -v cargo-audit >/dev/null 2>&1; then
  # cargo audit exits 1 on findings, so its status says nothing about whether
  # it ran; judge the output instead.
  (cd "$ROOT" && cargo audit --json) >"$WORK/audit.json" 2>"$WORK/audit.err" || true
  if jq -e 'has("vulnerabilities")' "$WORK/audit.json" >/dev/null 2>&1; then
    jq -r '.vulnerabilities.list[]
           | (.versions.patched // [] | join(", ")) as $fix
           | "| \(.package.name) | \(.package.version) | \(if $fix == "" then "no fixed release" else $fix end) | [\(.advisory.id)](https://rustsec.org/advisories/\(.advisory.id).html): \(.advisory.title) |"' \
      "$WORK/audit.json" >"$adv"
    advisories=$(jq '.vulnerabilities.list | length' "$WORK/audit.json")
    # Unmaintained, unsound and yanked crates; a yanked crate has no advisory.
    jq -r '(.warnings // {}) | to_entries[] | .key as $kind | .value[]
           | "| \(.package.name) | \(.package.version) | \($kind) | \(if .advisory then "[\(.advisory.id)](https://rustsec.org/advisories/\(.advisory.id).html): \(.advisory.title)" else "—" end) |"' \
      "$WORK/audit.json" >"$warn"
  else
    note_broken 'cargo audit' "$WORK/audit.err"
    audit_note='_`cargo audit` failed; see the banner above._'
  fi
else
  note_missing 'cargo-audit'
  audit_note='_Not collected: `cargo-audit` is not installed._'
fi

# ------------------------------------------------------------------- outdated

old=$WORK/old.md
: >"$old"
outdated_note=''
if command -v cargo-outdated >/dev/null 2>&1; then
  (cd "$ROOT" && cargo outdated --root-deps-only --format json) \
    >"$WORK/outdated.json" 2>"$WORK/outdated.err" || true
  if jq -e 'has("dependencies")' "$WORK/outdated.json" >/dev/null 2>&1; then
    # cargo-outdated writes "---" where there is no upgrade.
    jq -r '.dependencies[]
           | (if .kind == "Development" then " (dev)" elif .kind == "Build" then " (build)" else "" end) as $kind
           | [(.name + $kind), .project, .compat, .latest]
           | map(if . == null or . == "---" then "—" else . end)
           | "| \(.[0]) | \(.[1]) | \(.[2]) | \(.[3]) |"' \
      "$WORK/outdated.json" >"$old"
  else
    note_broken 'cargo outdated' "$WORK/outdated.err"
    outdated_note='_`cargo outdated` failed; see the banner above._'
  fi
else
  note_missing 'cargo-outdated'
  outdated_note='_Not collected: `cargo-outdated` is not installed._'
fi

# ----------------------------------------------------------------------- body

# The heartbeat line. "clean" is reserved for a run that looked everywhere; one
# that could not must not read the same as a quiet week.
if [ "$advisories" -eq 1 ]; then
  status="1 advisory"
else
  status="$advisories advisories"
fi
if [ -z "$broken$missing" ]; then
  [ "$advisories" -ne 0 ] || status=clean
else
  [ -z "$broken" ] || status="$status; report incomplete, $broken failed"
  [ -z "$missing" ] || status="$status; $missing not installed"
fi

printf '%s\n' "$MARKER"
printf '**last run: %s — %s**\n\n' "$(date -u '+%Y-%m-%d %H:%M UTC')" "$status"

if [ -n "$broken" ]; then
  printf '> **This report is incomplete.** `%s` could not be run, so the sections below are missing findings; the workflow run failed with it.\n' "$broken"
  for err in $broken_errs; do
    stderr_excerpt "$err"
  done
  printf '\n'
fi
if [ -n "$missing" ]; then
  printf '> **Partial report.** Not installed in the CI image: `%s`. The matching sections below are empty. Add the tool to `.forgejo/ci-image/Containerfile` and rebuild the image on the runner host with `.forgejo/ci-image/build.sh`.\n\n' "$missing"
fi

cat <<'INTRO'
Advisories come first: they are the part that needs a decision. Everything under
them is upgrade housekeeping.

This issue is rewritten in place every run and is never closed, so it stays the
same length and the heartbeat line above is the whole health check: a stale
timestamp means the schedule is broken, not that the week was quiet.

## Advisories

INTRO
if [ -n "$audit_note" ]; then
  printf '%s\n\n' "$audit_note"
else
  table '| crate | installed | fixed in | advisory |' '|---|---|---|---|' "$adv" \
    '_None._ `cargo audit` reads the whole lockfile and does not separate dev-dependencies, so anything appearing here is worth reading.'
fi

printf '## Unmaintained, unsound and yanked crates\n\n'
if [ -n "$audit_note" ]; then
  printf '%s\n\n' "$audit_note"
else
  table '| crate | installed | kind | advisory |' '|---|---|---|---|' "$warn" '_None._'
fi

printf '## Outdated direct dependencies\n\n'
if [ -n "$outdated_note" ]; then
  printf '%s\n\n' "$outdated_note"
else
  table '| crate | current | compatible | latest |' '|---|---|---|---|' "$old" '_Every direct crate is on its latest release._'
fi

if [ -n "${RUN_URL:-}" ]; then
  run_ref="[this workflow run]($RUN_URL)"
else
  run_ref='a manual run of `.forgejo/scripts/dependency-report.sh`'
fi
if [ -n "${SOURCE:-}" ]; then
  source_ref="[\`.forgejo/workflows/deps.yml\`]($SOURCE)"
else
  source_ref='`.forgejo/workflows/deps.yml`'
fi
printf -- '---\n\n'
printf 'Produced by %s, from %s.\n' "$run_ref" "$source_ref"

[ -z "$broken" ] || exit 1
