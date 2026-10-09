#!/usr/bin/env sh
# Run `cargo test` repeatedly and print the Markdown body of the evergreen
# flaky-test report on stdout. flaky.yml runs it nightly on main and hands the
# body to publish-report.sh; it also runs from a checkout.
#
#   flaky-report.sh <rounds>
#
# Exit status:
#   0  every round passed
#   1  a test failed in some round; the body lists it
#   2  a round produced no test results (the build broke); the body says so
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

broken=0
: >"$WORK/panics"
round=1
while [ "$round" -le "$ROUNDS" ]; do
  log=$WORK/round-$round.log
  # Failures are the findings, so the status is read from the log instead.
  # shellcheck disable=SC2086
  CARGO_TERM_COLOR=never cargo test --locked --no-fail-fast ${CARGO_TEST_ARGS:-} \
    >"$log" 2>&1 || true
  grep -q '^test result: ' "$log" || broken=$((broken + 1))
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
  round=$((round + 1))
done

cat "$WORK"/failed-* | sort | uniq -c | sort -rn >"$WORK/counts"

printf '%s\n' "$MARKER"
printf 'Nightly stress run: `cargo test` %s times on `main` at %s.\n\n' \
  "$ROUNDS" "$(git rev-parse --short HEAD 2>/dev/null || echo 'an unknown commit')"
if [ "$broken" -gt 0 ]; then
  printf '> [!WARNING]\n> %s of %s rounds produced no test results; the build or the test harness broke. The run log says why.\n\n' \
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
  printf 'No test failed%s.\n\n' "$([ "$broken" -eq 0 ] || echo ' in the rounds that produced results')"
fi
footer="updated $(date -u '+%Y-%m-%d %H:%M UTC')"
[ -z "${SOURCE:-}" ] || footer="[\`.forgejo/workflows/flaky.yml\`]($SOURCE) · $footer"
[ -z "${RUN_URL:-}" ] || footer="[run log]($RUN_URL) · $footer"
printf -- '---\n%s\n' "$footer"

if [ "$broken" -gt 0 ]; then exit 2; fi
if [ -s "$WORK/counts" ]; then exit 1; fi
exit 0
