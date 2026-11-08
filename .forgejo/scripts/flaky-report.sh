#!/usr/bin/env sh
# Run `cargo nextest run` repeatedly and print the Markdown body of the
# evergreen flaky-test report on stdout. flaky.yml runs it nightly on main and
# hands the body to publish-report.sh; it also runs from a checkout. Each failed
# test's captured output goes to stderr, so the run log keeps it after the round
# logs are gone.
#
#   flaky-report.sh <rounds>
#
# Exit status:
#   0  every round passed
#   1  a test failed in some round; the body lists it
#   2  a round failed without a failing test to account for it (the build
#      broke, or nextest itself failed or was stopped); the body says so
# Publish the body first in every case, then fail the job.
#
# Optional: RUN_URL and SOURCE are linked in the footer; NEXTEST_ARGS is
# passed to every `cargo nextest run`.
# Backticks in printf formats are Markdown, not command substitutions.
# shellcheck disable=SC2016
set -eu

MARKER='<!-- shoal:flaky-test-report -->'
[ $# -eq 1 ] && [ "$1" -gt 0 ] 2>/dev/null || {
  echo "usage: $0 <rounds>" >&2
  exit 2
}
ROUNDS=$1
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# Nextest's line for each test that ended, `STATUS [time] (n/total) binary
# test`: a failure is every status but PASS and LEAK, which passed. SLOW and
# TERMINATING lines carry no count. awk reads it from the environment, which
# keeps its backslashes.
FINISHED='^ *[A-Z][A-Z0-9-]* \[[^]]*\] \( *[0-9]+/[0-9]+\) '
export FINISHED

# Every test that failed in a round's log once, as `binary test`.
#   failed_tests <log>
failed_tests() {
  grep -E "$FINISHED" "$1" | grep -Ev '^ *(PASS|LEAK) ' |
    sed -E 's/^[^)]*\) //' | sort -u
}

# Whether a round's nextest status is more than its failed tests explain:
# nextest exits with 100 when tests failed, and with other statuses when the
# build broke or nextest itself failed or was stopped.
#   round_broken <log> <nextest status>
round_broken() {
  [ "$2" -ne 0 ] || return 1
  [ "$2" -eq 100 ] || return 0
  [ -z "$(failed_tests "$1")" ]
}

broken=0
: >"$WORK/panics"
round=1
while [ "$round" -le "$ROUNDS" ]; do
  log=$WORK/round-$round.log
  # Failures are the findings; the status only tells whether something else broke.
  status=0
  # shellcheck disable=SC2086
  CARGO_TERM_COLOR=never cargo nextest run --locked --profile ci --no-fail-fast ${NEXTEST_ARGS:-} \
    >"$log" 2>&1 || status=$?
  if round_broken "$log" "$status"; then
    broken=$((broken + 1))
    echo "round $round of $ROUNDS broke: nextest exited with status $status" >&2
    echo "::group::round $round end of log" >&2
    tail -n 100 "$log" >&2
    echo "::endgroup::" >&2
  fi
  failed_tests "$log" >"$WORK/failed-$round"
  # The first line of each failed test's first panic message, from the
  # output nextest prints, indented, after the test's line.
  awk '
    $0 ~ ENVIRON["FINISHED"] {
      test = $0
      sub(/^[^)]*\) /, "", test)
      next
    }
    test != "" && !(test in seen) && /^    thread .* panicked at / {
      where = $0
      sub(/.* panicked at /, "", where)
      sub(/:$/, "", where)
      getline message
      sub(/^    /, "", message)
      seen[test] = 1
      printf "%s\t%s: %s\n", test, where, message
    }' "$log" >>"$WORK/panics"
  echo "round $round of $ROUNDS: $(grep -c . "$WORK/failed-$round" || true) failed" >&2
  if [ -s "$WORK/failed-$round" ]; then
    echo "::group::round $round failure output" >&2
    # Each failed test's output, bounded: from the summary, which repeats
    # them all, or, in a round stopped before it, from each test's line.
    summary=0
    grep -q '^ *Summary \[' "$log" && summary=1
    awk -v max=200 -v summary="$summary" '
      /^ *Summary \[/ { reached = 1; print; next }
      summary && !reached { next }
      $0 ~ ENVIRON["FINISHED"] {
        failed = $0 !~ /^ *(PASS|LEAK) /
        lines = 0
        if (failed) print
        next
      }
      failed {
        if (++lines <= max) print
        else if (lines == max + 1) print "[output truncated]"
      }' "$log" >&2
    echo "::endgroup::" >&2
  fi
  round=$((round + 1))
done

cat "$WORK"/failed-* | sort | uniq -c | sort -rn >"$WORK/counts"

printf '%s\n' "$MARKER"
printf 'Nightly stress run: `cargo nextest run` %s times on `main` at %s.\n\n' \
  "$ROUNDS" "$(git rev-parse --short HEAD 2>/dev/null || echo 'an unknown commit')"
if [ "$broken" -gt 0 ]; then
  printf '> [!WARNING]\n> %s of %s rounds failed without a failing test to account for it: the build broke, or nextest itself failed or was stopped. The run log says why.\n\n' \
    "$broken" "$ROUNDS"
fi
if [ -s "$WORK/counts" ]; then
  printf 'A test listed here failed on unchanged code, so it is flaky. Open an issue with its log excerpt and fix the cause (AGENTS.md, CI).\n\n'
  printf '| Test | Failed rounds | First failure |\n| --- | --- | --- |\n'
  while read -r count name; do
    first=$(awk -F '\t' -v n="$name" '$1 == n { print $2; exit }' "$WORK/panics" |
      sed 's/|/\\|/g; s/`//g' | cut -c1-200)
    printf '| `%s` | %s of %s | %s |\n' "$name" "$count" "$ROUNDS" "${first:+\`$first\`}"
  done <"$WORK/counts"
  printf '\n'
else
  printf 'No test failed.\n\n'
fi
footer="updated $(date -u '+%Y-%m-%d %H:%M UTC')"
[ -z "${SOURCE:-}" ] || footer="[\`.forgejo/workflows/flaky.yml\`]($SOURCE) · $footer"
[ -z "${RUN_URL:-}" ] || footer="[run log]($RUN_URL) · $footer"
printf -- '---\n%s\n' "$footer"

if [ "$broken" -gt 0 ]; then exit 2; fi
if [ -s "$WORK/counts" ]; then exit 1; fi
exit 0
