# Profiling

This file covers the contributor tools that measure where Shoal and its tests
spend time. The user documentation leaves them out.

## Commands

`python3 scripts/profile_shoal.py [--setup ARGS]... -- <shoal args>` times a
command against a foreground daemon in a temporary home and lists where the
time went. `--help` shows its options. It builds the `profiling` Cargo
profile: release code with symbols, for samplers such as samply.

`SHOAL_PROFILE=1` makes a Shoal process print a `profile` line to stderr for
each daemon call, daemon request, daemon startup step and subprocess run.

## Tests

`python3 scripts/profile_tests.py suite` runs each test binary whole, one after
another, and prints its wall, user and system time.

`python3 scripts/profile_tests.py tests --binary NAME` runs every test of a
binary alone, as nextest does, and lists the costliest.

Both count a test's daemons and commands, but not processes that outlive it.
`--git` counts their Git processes, keeping production code's apart from the
fixtures'.
