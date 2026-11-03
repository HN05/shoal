#!/usr/bin/env sh
# Run `cargo test` repeatedly and print the Markdown body of the evergreen
# flaky-test report on stdout. flaky.yml runs it nightly on main and hands the
# body to publish-report.sh; it also runs from a checkout. Each failed test's
# captured output goes to stderr, so the run log keeps it after the round logs
# are gone.
#
#   flaky-report.sh <rounds>
#
# Exit status:
#   0  every round passed
#   1  a test failed in some round; the body lists it
#   2  a round failed without a failing test to account for it (the build
#      broke, or a test binary crashed or was killed); the body says so
# Publish the body first in every case, then fail the job.
#
# Optional: RUN_URL and SOURCE are linked in the footer; CARGO_TEST_ARGS is
# passed to every `cargo test`.
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

# Whether a round's cargo status is more than its failed tests explain: no
# binary reported results, cargo itself was stopped (it exits 101 for every
# failure it reports), or a binary failed without a FAILED summary because it
# crashed or was killed before printing one.
#   round_broken <log> <cargo status>
round_broken() {
  grep -q '^test result: ' "$1" || return 0
  [ "$2" -ne 0 ] || return 1
  [ "$2" -eq 101 ] || return 0
  failed_targets=$(grep -c '^error: .*failed, to rerun pass ' "$1" || true)
  failed_summaries=$(grep -c '^test result: FAILED\.' "$1" || true)
  [ "$failed_summaries" -eq 0 ] || [ "$failed_summaries" -lt "$failed_targets" ]
}

# The end of each part of a broken round's log that reported an error but no
# test results: the build's compiler errors, or a crashed binary's last tests
# and signal. Without one, the end of the log, where cargo stopped.
print_round_errors() {
  awk -v keep=100 '
    function flush(  i) {
      if (failed && !summary) {
        if (n > keep) print "[" n - keep " earlier lines omitted]"
        for (i = n > keep ? n - keep + 1 : 1; i <= n; i++) print lines[i % keep]
        shown = 1
      }
      n = 0; failed = 0; summary = 0
    }
    /^ *(Running|Doc-tests) / { flush() }
    { lines[++n % keep] = $0 }
    /^error/ { failed = 1 }
    /^test result: / { summary = 1 }
    END {
      if (!shown) { failed = 1; summary = 0 }
      flush()
    }' "$1"
}

broken=0
: >"$WORK/panics"
round=1
while [ "$round" -le "$ROUNDS" ]; do
  log=$WORK/round-$round.log
  # Failures are the findings; the status only tells whether something else broke.
  status=0
  # shellcheck disable=SC2086
  CARGO_TERM_COLOR=never cargo test --locked --no-fail-fast ${CARGO_TEST_ARGS:-} \
    >"$log" 2>&1 || status=$?
  if round_broken "$log" "$status"; then
    broken=$((broken + 1))
    echo "round $round of $ROUNDS broke: cargo exited with status $status" >&2
    echo "::group::round $round errors" >&2
    print_round_errors "$log" >&2
    echo "::endgroup::" >&2
  fi
  # Every failed test once per round; helper processes print their own lines.
  sed -n 's/^test \(.*\) \.\.\. FAILED$/\1/p' "$log" | sort -u >"$WORK/failed-$round"
  # The first line of each failed test's panic message.
  awk '
    /^thread .* panicked at / {
      name = $0
      sub(/^thread \047/, "", name)
      sub(/\047.*/, "", name)
      where = $0
      sub(/.* panicked at /, "", where)
      sub(/:$/, "", where)
      getline message
      if (!(name in seen)) {
        seen[name] = 1
        printf "%s\t%s: %s\n", name, where, message
      }
    }' "$log" >>"$WORK/panics"
  echo "round $round of $ROUNDS: $(grep -c . "$WORK/failed-$round" || true) failed" >&2
  if [ -s "$WORK/failed-$round" ]; then
    echo "::group::round $round failure output" >&2
    # Every captured section up to its binary's summary, each one bounded.
    awk -v max=200 '
      /^---- .* std(out|err) ----$/ { printing = 1; lines = 0; print; next }
      /^failures:$/ { printing = 0 }
      printing {
        if (++lines <= max) print
        else if (lines == max + 1) print "[output truncated]"
      }' "$log" >&2
    echo "::endgroup::" >&2
  fi
  round=$((round + 1))
done

cat "$WORK"/failed-* | sort | uniq -c | sort -rn >"$WORK/counts"

printf '%s\n' "$MARKER"
printf 'Nightly stress run: `cargo test` %s times on `main` at %s.\n\n' \
  "$ROUNDS" "$(git rev-parse --short HEAD 2>/dev/null || echo 'an unknown commit')"
if [ "$broken" -gt 0 ]; then
  printf '> [!WARNING]\n> %s of %s rounds failed without a failing test to account for it: the build broke, or a test binary crashed or was killed. The run log says why.\n\n' \
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
