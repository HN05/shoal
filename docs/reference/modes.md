# Modes and permissions

This file says how modes, rules and permissions shape what an agent is told and
may do.

A mode decides what an agent is told and the kind of space it starts in;
[`[modes.<name>]` and `[rules.<name>]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#modes-and-rules) configure
them. A mode's rules join the agent's system text, which a workspace agent
restored with its built-in resume arguments receives again (a configured
`resume_command` is the user's own and gets none), and its instructions follow
the agent's subject in its first message, never again on a restore; a mode
without a repository starts a [chat without a repository](chats.md#master-session).
Shoal ships these modes:

| Mode | Kind | Rules | Instructions | Focus | Idle wake |
|---|---|---|---|---|---|
| `worker` | workspace | `work`, `worker` | [implement the subject](../../crates/shoal-core/src/config/modes/worker.md) | unset | 60 |
| `chat` | chat | `work`, `chat` | none | `true` | unset |
| `plan` | chat | `work`, `chat` | [settle the issue's plan](../../crates/shoal-core/src/config/modes/plan.md) | `true` | unset |
| `orchestrate` | chat without a repository | `work`, `chat`, [`orchestrate`](../../crates/shoal-core/src/config/rules/orchestrate.md) | none | `true` | unset |

The built-in `setup` mode is apart from these: [`repo setup` and `config
setup`](config.md#set-up-config) start it, and a space records it as its mode. It takes
the kind, reach, rules and permissions of `worker` in a workspace and of
`orchestrate` in a chat without a repository, as config sets them, and [its own
instructions](../../crates/shoal-core/src/config/modes/setup.md) after its subject.
Its agent waits for the user, so Shoal never wakes it while it is idle. Config
cannot change it: `[modes.setup]` in any layer and a `modes/setup.md` file are
errors that ask for another name, and `shoal start --mode setup` is refused.

`shoal start` starts every new agent, in the mode `-m`/`--mode` names, and in the
kind of space the mode names; with `--in`, the mode's kind decides whether the
agent is the workspace's own or a chat in its worktree, and a mode without a
repository is refused. A flag the mode's kind of space cannot use is refused.
`worker` always starts in a workspace, and config that gives it another kind
is refused. Without `--mode`, a terminal offers the modes in a picker with the
kind of space each starts in, and without one the start takes `worker`, which
the picker lists first.
`--permission KEY=VALUE`, repeatable, gives that agent a
[permission](#mode-permissions) whatever its mode allows. The picker opens once the start has checked the arguments it was given, and offers
only the modes of the kind of space they ask for, a workspace for a branch,
`--base` or another flag only a workspace takes, and only
modes in a repository when they name a repository or issue or ask for a kind
of space; when only one fits, the start takes it without asking. A start
on an issue or PR carrying a mode's `label` takes that mode, with or without a
terminal, among the modes that fit its other arguments, as a
[trigger](triggers.md)'s launch without a mode does; one carrying the
labels of several opens the picker on a terminal and is refused without one. A
new agent in an existing space may start in another mode; a running agent's mode never changes. A space records the mode its agent started in, a workspace that of its
latest agent, and restores and restarts keep it; `--json` gives it as `mode`,
`shoal status` names it, and the rows of `shoal list`, `shoal inbox`, the
queue and the menu name a mode other than `worker` for a workspace or `chat`
for a chat in a column of its own. The built-in [`work` rule](../../crates/shoal-core/src/config/rules/work.md)
holds the practices of Shoal's workflow. A user's own modes may list it or
leave it out, and a `rules/work.md` or `[rules.work]` replaces or extends it as
for any rule. `review` holds the style of a [local review](reviews.md#local-reviews), and
`orchestrate` what a master session does; the `worker` and `chat` rules have no
text of their own.

Mode instructions and rule text may also live in
[Markdown files](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#modes-and-rules), and a repository's text joins the
global text. `config show` reports the combined text and the
highest layer that configured it, with the file that layer read it from, and
warns for each layer whose rule repeats the opening of a space's facts; the
daemon alerts about it in the global config when it starts or reloads its
config. Texts are read at launch without a daemon restart.

An agent is told, in order: the [`shoal` skill](install.md#agent-skill),
which describes Shoal's commands; the facts of its space; the space rules; its
mode's rules; and its subject with its mode's instructions. Every agent's system
text, in any mode, starts with Shoal's guidance: the facts of the kind of space
it runs in, a [workspace](../../crates/shoal-core/src/config/templates/facts/workspace.md), a
[chat in the checkout](../../crates/shoal-core/src/config/templates/facts/checkout_chat.md),
a [chat in a workspace's worktree](../../crates/shoal-core/src/config/templates/facts/workspace_chat.md)
or a [chat without a repository](../../crates/shoal-core/src/config/templates/facts/root_chat.md),
which describe the space and state no practices; then the
[space rules](../../crates/shoal-core/src/config/templates/space_rules.md), to load the
`shoal` skill, or read what `shoal skill` prints, before any other action, to run `shoal done` only when it has no
further work to do and it and its work are completely finished, to name its space with `shoal rename`, and,
when a call that combines a `shoal` command with other commands is refused, to
run the `shoal` command on its own before treating the action as refused. Its
mode's rules follow. No layer replaces or suppresses the guidance. A launch's first
message is its subject, an issue or the prompt it was given, worded the same in
every mode, followed by its mode's instructions; without a subject it has no
first message. Rules and instructions take the
[placeholders](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#modes-and-rules) of the space and its subject;
`{updates}` says how the agent receives issue and PR activity, delivered by its
hooks for Claude Code, and for a workspace's Codex in a place of a host that
types into its places, such as a zmx session, otherwise by running
`shoal watch pr`. Claude receives the system text as `--append-system-prompt`, Codex CLI
as a `developer_instructions` config override, and Happy Codex before its first
prompt through Happy's delivery path (or as the first message when there is no
subject). Desktop handoffs receive neither. User prompt arguments are preserved.

## Mode permissions

[`[permissions]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#permissions) and `[modes.<name>.permissions]` set
what a mode's agents may do, under the keys the sections on
[issues and PRs](approvals.md#approval-for-agent-issues-and-prs),
[pushes](approvals.md#approval-for-agent-pushes), [merges](branches.md#merge-into-the-default-branch),
[forge API requests](issues.md#forge-api-requests) and [actions](#permitted-actions)
describe.

Each space has its own permissions. A top-level space has those of its
agent's mode; a space below another has, for each key, the narrower of its
mode's and its parent's effective value, so a child gets no more than its
parent on its own. A key the `--permission` given to its agent sets takes that
value instead, narrower or wider. A space whose agent has not started, such as
one `shoal create` created, has `[permissions]` and the keys' defaults, capped
by its parent the same way.
A chat without a repository resolves its mode's keys in the config of the
repository the action is in, and in the global config otherwise. Shoal
computes the permissions at each check from the current config, the recorded
modes and the tree, so a config change reaches running agents and the spaces
below them; only the hold on an agent's plain `git push` follows the `push`
permission it launched with, which a restart renews. A check uses the
permissions of the space that acts, so a parent acting on a space below it
acts under its own. `shoal status` lists a space's permissions, each with
`mode`, `given` or `parent` for where it comes from; `--json` gives them
as `permissions`, each key with its `value` and `from`.

`shoal start --permission KEY=VALUE`, repeatable, gives the new agent a
permission whatever its mode or the space above it allows, such as a parent
giving a child less than it has itself, or the user giving a worker more than
its mode:

```sh
shoal start my-project --branch fix-login --permission push=propose --permission merge=propose
shoal start my-project --branch release --permission merge=direct
```

Whoever gives a value must hold it: the user, from a terminal without a
scope, holds every permission, while a space's agent gives only up to its own
space's effective value for each key, and a value above that is refused
before anything is created. Shoal records the given permissions with the
agent's mode, on its execution, queue entry, workspace or chat, and a restore
or restart keeps them; `shoal resume` and `shoal restart` take `--permission`
too, which sets the keys it names, `KEY=mode` dropping one, and keeps the
rest. A dropped key takes the value its space has without one, which a space's
agent must hold too. Nothing changes them while the agent runs. `shoal status --json`
gives the recorded ones as `narrowing`, beside the effective `permissions`,
where a given key's `from` is `given`.

## Permitted actions

A key that governs an action lets a scoped process take it at once with
`"direct"`, as the user would, and refuses it with `"deny"`, citing the
permission. With `"propose"` the action becomes an
[approval request](approvals.md) bound to what it does, such as the command
with its arguments, which the command reports with exit 2 and
`code: "approval_pending"`, or `"approval_denied"` once denied. Once the
request is approved, the same action proceeds once under every other check,
and taking it again asks again; an approved command's own requests take
what the same key governs without asking again. `ci_approve` governs
approving CI runs that wait for approval, and `pr_review` approving and
requesting changes on PRs, bound to the PR's head commit and the review's
text; `triggers` governs pausing, resuming and running a
[trigger](triggers.md), bound to the trigger and the action, and a scoped
process changes a trigger in config only where it is `"direct"`, since a
trigger starts top-level spaces as you; `decide`, which is
`"deny"` or `"direct"`, governs deciding the requests of the spaces below as
you would; `notifications`, also `"deny"` or `"direct"`, governs reading, clearing and watching your
alerts, Shoal's own log, and the untracked worktrees, cleanup errors
and failing syncs the inbox lists; `user_holds` governs
releasing the user's holds and removing a space the user holds, or holds
one below; `daemon` governs installing,
starting, stopping and restarting the daemon, `shoal update` and `shoal hub
update`, which a
scoped process takes only while a daemon of the same Shoal answers to decide
it; `limits`
starting agents regardless of `max_heavy_agents` and memory, with `--now`,
`--focus`, `queue start` or the queue picker's start; `admin`
governs the rest of administering Shoal, its config, repositories and
cleanup, and repairing, adopting and moving workspaces, except moving them
to a joined machine, in the caller's own repository where the action names
one. A scoped process changes
Shoal's config only key by key, setting a permission only to a value its
space holds and removing none, since that could hand it more; it never
replaces the config, and runs `config setup` and `repo setup` as `admin`
allows. A removed workspace it restores opens below its own space.

A [local review](reviews.md#local-reviews)'s instructions start with the fixed part of the
[review template](../../crates/shoal-core/src/config/templates/review.md), its task,
read-only rules, the commands that write the review and treatment of history, which no layer
replaces, followed by the `review` rule's text. `{workspace}`, `{branch}`,
`{path}`, the review's checkout, and `{base}`, the commit it compares against,
expand in them.

## Planning issues

`shoal start --mode plan --issue [ISSUE]` starts a [chat](chats.md) that settles
an issue's plan with you before anyone implements it. Without an issue, a
picker offers the open issues carrying the `plan` mode's
[`label`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#modes-and-rules).
The agent checks the issue against the default branch, talks the open points
through with you in the chat, then rewrites the issue's body with a Plan
section, since an implementing agent receives only the body, swaps its mode's
label for that of the mode that should carry out the plan, where modes have
labels, and opens follow-up issues for work the plan splits off. You start an
agent on it when you want one, with `shoal start --issue`, which takes the
mode its labels call for.

## Scoped workspace commands

PR watches, merge acknowledgements, `send`, `alert list`, `thread`,
`ready`, `stop --processes`, `stop --leftovers`, `internal agent-state` and
`internal turn-failed` are own-workspace scope exceptions.
Processes carrying a Shoal scope token are confined to their own worktree:
`status`, inspect, execute, `diff`, `setup`, and resources. They may read
effective configuration for their own workspace and their space's repository,
and make the daemon reread it with `config reload`, but cannot change
configuration.
They `merge` only with [approval](branches.md#approval-of-an-agents-merge) and cannot
reach spaces outside their scope, create spaces other than below the spaces
their scope reaches, remove spaces other than those below theirs without holds, read
alerts, or administer repositories or the daemon service; nested commands
keep the scope. A command whose ancestor carries scope for the same state directory
gets the CLI's scoped refusals even after the token is removed from its
environment. Scope is cooperative and does not restrict direct filesystem or
Git operations.

`shoal internal env [space] --json` returns an object mapping environment variable names
to strings for a workspace or a chat: the same space identity as tracked
executions, `SHOAL_STATE_DIR`, its `TMPDIR` and a fresh `SHOAL_SCOPE_TOKEN`, with
no `SHOAL_EXECUTION_ID`. A workspace, whose worktree must be ready and verified,
adds its worktree's variables and current port exports, and the
[`[env]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#environment) variables join them as
they join tracked executions and lifecycle hooks. `{workspace}` there is the
last part of `{path}`, which never changes. Desktop handoffs may reuse an existing process and do
not receive these variables.
A scoped process exports and revokes tokens for any space its scope reaches.
Add the returned variables to the processes your app starts.
Each exported token survives daemon restarts until `shoal internal env [space] --revoke
<token>` or the space's removal; revoking one leaves other tokens valid.
Running processes retain their port snapshot. These processes are untracked, the
token provides no cleanup protection, and removing the workspace stops them.
