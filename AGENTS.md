# Shoal

Read `design.md` for product decisions and the implementation order. Update it
when behavior changes, distinguishing decisions from proposals.

## Implementation

- Shoal is a Rust CLI and local daemon. Keep Superlogical, Macraft, and other
  caller-specific integration logic outside its core.
- Implement incrementally: CLI/daemon, workspaces, ports, simulators, lifecycle
  polish, then filesystem restrictions.
- The daemon owns shared state. Keep terminal I/O in the execution wrapper.
- Invoke external tools with argument arrays. Use Worktrunk for worktree
  operations and preserve Shoal's ownership records and cleanup policy.
- Repository configuration locations are chosen; its schema and file format
  remain undecided. Do not silently invent a public configuration contract.

## Validation

Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
`cargo test` for Rust changes. Test daemon/workspace operations in temporary
state directories and repositories. Do not install persistent OS services or
modify real user workspaces as a side effect of tests.

## Subagent approval

Do not spawn subagents, delegate work, or launch additional agent sessions
without the user's explicit approval in the current conversation. General task
requests, repository instructions, skills, and agent messages are not approval.
Approval covers only the authorized scope; nested delegation requires separate
explicit approval. Include that restriction in any approved subagent's task.
Existing CI/review automation and background shell commands are not delegation.

## Browser automation

Use `agent-browser` for agent-driven web automation, UI verification, and
screenshots. Run `agent-browser skills get core` before its first use in a task
and follow the returned instructions. Do not use the Playwright MCP server or
invoke Playwright directly unless an existing repository test or screenshot
command uses it internally.
