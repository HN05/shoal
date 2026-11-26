# Modes and permissions

This file says how modes, rules and permissions shape what an agent is told and
may do. [Modes and guidance](../design/modes.md) and
[permissions](../design/permissions.md) give the reasons.

## Modes

A mode decides what an agent is told and the kind of space it starts in.
[`[modes.<name>]` and `[rules.<name>]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#modes-and-rules)
configure modes and the rules they list. A mode without a repository starts a
[chat without a repository](chats.md#master-session).

Shoal ships these modes:

| Mode | Kind | Rules | Instructions | Focus | Idle wake |
|---|---|---|---|---|---|
| `worker` | workspace | `work`, `worker` | [implement the subject](../../crates/shoal-core/src/config/modes/worker.md) | unset | 60 |
| `chat` | chat | `work`, `chat` | none | `true` | unset |
| `plan` | chat | `work`, `chat` | [settle the issue's plan](../../crates/shoal-core/src/config/modes/plan.md) | `true` | unset |
| `orchestrate` | chat without a repository | `work`, `chat`, [`orchestrate`](../../crates/shoal-core/src/config/rules/orchestrate.md) | none | `true` | unset |

Shoal also ships these rules:

- [`work`](../../crates/shoal-core/src/config/rules/work.md) holds the
  practices of Shoal's workflow. A user's own modes may list it or leave it
  out. A `rules/work.md` file or a `[rules.work]` table replaces or extends it,
  as for any rule.
- `review` holds the style of a [local review](#local-review-instructions).
- `orchestrate` says what a master session does.
- `worker` and `chat` have no text of their own.

## The setup mode

[`repo setup` and `config setup`](config.md#set-up-config) start an agent in
the built-in `setup` mode. The space records `setup` as its mode.

The setup mode takes the kind, reach, rules and permissions that config gives
another mode:

- in a workspace, those of `worker`;
- in a chat without a repository, those of `orchestrate`.

It then adds [its own instructions](../../crates/shoal-core/src/config/modes/setup.md)
after its subject. Its agent waits for you, so Shoal never wakes it while it is
idle.

Config cannot change the setup mode. A `[modes.setup]` table in any layer is an
error that asks for another name, and so is a `modes/setup.md` file.
`shoal start --mode setup` is refused.

## Start in a mode

`shoal start` starts every new agent, in the mode `-m`/`--mode` names. The mode
alone decides the kind of space the agent starts in. A flag that kind of space
cannot use is refused. `worker` always starts in a workspace, and config that
gives it another kind is refused.

With `--in`, the mode's kind decides the agent's role in the existing
workspace. A mode that starts in a workspace starts the workspace's own agent.
A mode that starts in a chat starts a chat in its worktree. A mode without a
repository is refused.

`--permission KEY=VALUE` gives the new agent a permission, as
[giving permissions](#give-an-agent-permissions) says.

### Without `--mode`

A start on an issue or PR that carries a mode's `label` takes that mode, with
or without a terminal. It chooses among the modes that fit its other arguments.
A [trigger](triggers.md)'s launch without a mode chooses the same way. An item
that carries the labels of several modes opens the picker on a terminal, and is
refused without one.

Otherwise a terminal offers the modes in a picker, with the kind of space each
starts in. Without a terminal the start takes `worker`, which the picker lists
first.

The picker opens once the start has checked the arguments it was given. It
offers only the modes those arguments fit:

- only modes of the kind of space they ask for, such as a workspace for a
  branch, `--base` or another flag only a workspace takes;
- only modes in a repository when they name a repository or an issue, or ask
  for a kind of space.

When only one mode fits, the start takes it without asking.

### The recorded mode

A space records the mode its agent started in. A workspace records the mode of
its latest agent. Restores and restarts keep the mode. A new agent in an
existing space may start in another mode, but a running agent's mode never
changes.

`shoal status` names the mode, and `--json` gives it as `mode`. The rows of
`shoal list`, `shoal inbox`, the queue and the menu show it in a column of its
own when it is not the kind's usual one: `worker` for a workspace, `chat` for a
chat.

## What an agent is told

An agent receives, in this order:

1. the [`shoal` skill](install.md#agent-skill), which describes Shoal's
   commands;
2. the facts of its space;
3. the space rules;
4. its mode's rules;
5. its subject, followed by its mode's instructions.

The facts and the space rules make up Shoal's guidance. Every agent's system
text starts with it, in any mode, and no layer replaces or suppresses it.

- The [facts](../../crates/shoal-core/src/config/templates/facts/) describe the
  kind of space the agent runs in, such as a workspace or a chat in the
  checkout. They state no practices.
- The [space rules](../../crates/shoal-core/src/config/templates/space_rules.md)
  hold the practices Shoal's own mechanics depend on, such as loading the
  `shoal` skill before any other action.

The mode's rules follow the guidance in the system text.

A launch's first message is its subject: an issue, or the prompt it was given.
The subject reads the same in every mode, and the mode's instructions follow
it. A launch without a subject has no first message.

### Restores

A workspace agent restored with its built-in resume arguments receives its
system text again. A configured `resume_command` is your own and receives
none. The mode's instructions come only in the first message, never again on a
restore.

### Placeholders

Rules and instructions take the
[placeholders](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#placeholders)
of the space and its subject. `{updates}` says how the agent receives issue and
PR activity:

- its hooks deliver it, for Claude Code, and for a workspace's Codex in a place
  of a host that types into its places, such as a zmx session;
- otherwise the agent runs `shoal watch pr`.

### How each tool receives it

- Claude receives the system text as `--append-system-prompt`.
- Codex CLI receives it as a `developer_instructions` config override.
- Happy Codex receives it before its first prompt, through Happy's delivery
  path. Without a subject it receives it as the first message.
- Desktop handoffs receive neither the system text nor the first message.

Shoal keeps the prompt arguments you give.

### Text in files

Mode instructions and rule text may also live in
[Markdown files](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#text-in-markdown-files).
A repository's text joins the global text.
[`config show`](config.md#inspect-effective-configuration) shows the combined
text and where it comes from.

## Local review instructions

A [local review](reviews.md#local-reviews)'s instructions start with the fixed
part of the [review template](../../crates/shoal-core/src/config/templates/review.md).
It holds the review's task, its read-only rules, the commands that write the
review and how to treat history. No layer replaces it. The `review` rule's text
follows it.

These placeholders expand in both:

- `{workspace}` and `{branch}` of the reviewed workspace;
- `{path}`, the review's checkout;
- `{base}`, the commit the review compares against.

## Mode permissions

[`[permissions]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#permissions)
and `[modes.<name>.permissions]` set what a mode's agents may do. The keys
govern [issues and PRs](approvals.md#approval-for-agent-issues-and-prs),
[pushes](approvals.md#approval-for-agent-pushes),
[merges](branches.md#approval-of-an-agents-merge),
[forge API requests](issues.md#forge-api-requests) and other
[actions](#permitted-actions).

Each space has its own permissions:

- A top-level space has those of its agent's mode.
- A space below another has, for each key, the narrower of its mode's value
  and its parent's effective value. A child so gets no more than its parent
  from its mode.
- A key [given](#give-an-agent-permissions) to its agent takes the given value
  instead, narrower or wider.
- A space whose agent has not started, such as one `shoal create` created, has
  `[permissions]` and the keys' defaults. Its parent caps them the same way.

A chat without a repository resolves its mode's keys in the config of the
repository the action is in. Outside a repository it resolves them in the
global config.

Shoal computes the permissions at each check from the current config, the
recorded modes and the tree. A config change so reaches running agents and the
spaces below them. One exception: the hold on an agent's plain `git push`
follows the `push` permission the agent launched with. A restart renews it.

A check uses the permissions of the space that acts. A parent that acts on a
space below it acts under its own.

`shoal status` lists a space's permissions, each with where its value comes
from: `mode`, `given` or `parent`. `--json` gives them as `permissions`, each
key with its `value` and `from`.

### Give an agent permissions

`shoal start --permission KEY=VALUE`, repeatable, gives the new agent a
permission whatever its mode or the space above it allows. A parent may give a
child less than it has, and you may give a worker more than its mode:

```sh
shoal start my-project --branch fix-login --permission push=propose --permission merge=propose
shoal start my-project --branch release --permission merge=direct
```

The giver must hold the value it gives. You, from a terminal without a scope,
hold every permission. A space's agent gives each key only up to its own
space's effective value. A value above that is refused before anything is
created.

Shoal records the given permissions with the agent's mode: on its execution,
queue entry, workspace or chat. Restores and restarts keep them, and nothing
changes them while the agent runs. `shoal status --json` gives them as
`narrowing`, beside the effective `permissions`, where a given key's `from` is
`given`.

`shoal resume` and `shoal restart` take `--permission` too. It sets the keys
it names and keeps the rest. `KEY=mode` drops a given key, which then takes the
value its space has without one. The giver must hold that value too.

## Permitted actions

A key that governs an action takes one of these values:

- `"direct"` lets a scoped process take the action at once, as you would.
- `"deny"` refuses the action, citing the permission.
- `"propose"` turns the action into an
  [approval request](approvals.md#approval-requests) bound to what it does,
  such as the command with its arguments.

Once the request is approved, the same action proceeds once, under every other
check. Taking it again asks again. The approved command's own requests take
what the same key governs without asking again.

The [configuration reference](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#permissions)
says which actions each key governs. Some keys bind their requests to more, or
apply only under a condition:

- `pr_review` binds the PR's head commit and the review's text.
- `triggers` binds the [trigger](triggers.md) and the action.
- `limits` covers `--now`, `--focus`, `queue start` and the queue picker's
  start.
- `daemon` lets a scoped process take its action only while a daemon of the
  same Shoal answers to decide it.
- `admin` covers an action in the caller's own repository where the action
  names one. A [move](move.md) to a joined machine takes no `admin`.

A scoped process changes Shoal's config only key by key. It sets a permission
only to a value its space holds, and removes none, since that could hand it
more. It never replaces the config. It runs `config setup` and `repo setup` as
`admin` allows. A removed workspace it restores opens below its own space.

## Planning issues

`shoal start --mode plan --issue [ISSUE]` starts a [chat](chats.md) that
settles an issue's plan with you before anyone implements it. Without an issue,
a picker offers the open issues that carry the `plan` mode's
[`label`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#modes-and-rules).

The agent:

1. checks the issue against the default branch;
2. talks the open points through with you in the chat;
3. rewrites the issue's body with a Plan section, since an implementing agent
   receives only the body;
4. swaps its mode's label for that of the mode that should carry out the plan,
   where modes have labels;
5. opens follow-up issues for work the plan splits off.

You start an agent on the issue when you want one. `shoal start --issue` takes
the mode its labels call for.

## Scoped workspace commands

A process that carries a Shoal scope token is confined to its own worktree for
`status`, inspect, execute, `diff`, `setup` and resources. It may still run
these on its own workspace: PR watches, merge acknowledgements, `send`,
`alert list`, `thread`, `ready`, `stop --processes`, `stop --leftovers`,
`internal agent-state` and `internal turn-failed`.

A scoped process reads the effective configuration of its own workspace and
its space's repository. It makes the daemon reread it with `config reload`, but
cannot change it.

A scoped process cannot:

- `merge` without [approval](branches.md#approval-of-an-agents-merge);
- reach spaces outside its scope;
- create spaces other than below the spaces its scope reaches;
- remove spaces other than those below its own without holds;
- read alerts;
- administer repositories or the daemon service.

Nested commands keep the scope. A command whose ancestor carries scope for the
same state directory gets the CLI's scoped refusals even after the token is
removed from its environment. Scope is cooperative: it does not restrict
direct filesystem or Git operations.

## Export a space's environment

`shoal internal env [space] --json` returns the environment for a process your
app starts in a workspace or a chat. It is an object that maps each variable's
name to a string:

- the same space identity as a tracked execution, without
  `SHOAL_EXECUTION_ID`;
- `SHOAL_STATE_DIR` and the space's `TMPDIR`;
- a fresh `SHOAL_SCOPE_TOKEN`;
- for a workspace, its worktree's variables and current port exports. The
  worktree must be ready and verified.
- the [`[env]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#environment)
  variables, as they join tracked executions and lifecycle hooks. `{workspace}`
  there is the last part of `{path}`, which never changes.

Add the returned variables to the processes your app starts. Desktop handoffs
may reuse an existing process and do not receive them.

A scoped process exports and revokes tokens for any space its scope reaches.
An exported token survives daemon restarts until
`shoal internal env [space] --revoke <token>` or the space's removal. Revoking
one leaves other tokens valid.

Running processes keep the port snapshot they started with. They are
untracked, and the token gives no cleanup protection. Removing the workspace
stops them.
