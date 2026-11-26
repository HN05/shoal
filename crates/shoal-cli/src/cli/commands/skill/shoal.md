---
name: shoal
description: What Shoal's commands do, for agents in a Shoal workspace or chat and for sessions outside one — start and coordinate agents, open, change, watch and review issues and PRs, mark work ready, ask the user, hand back files, finish with `shoal done`, sync the default branch, create workspaces without an agent, and reserve ports, simulators, containers, resource permits and secrets.
---

# Shoal

This skill says what Shoal's commands do, so that an agent can choose and run
them. `shoal <command> --help` lists every flag.

Commands print JSON and never prompt when `SHOAL_OUTPUT=json`. Shoal sets it
for the agents it launches. Elsewhere, pass `--json`.

## Spaces and scope

A space is a workspace or a chat. A workspace has its own worktree and branch;
a chat has neither. Spaces form a tree, and each has at most one parent.

An agent Shoal launches is scoped to its own space and the spaces below it.
Only the user's own terminal is unscoped. A master session is a chat without a
repository, which `shoal start --mode orchestrate` starts in `root_dir`. Its
scope reaches every space.

Inside a space, a command without a target acts on that space. In a child
workspace's directory, it acts on the child. Outside any space, it opens a
picker on a terminal. A session that Shoal did not launch, in a worktree Shoal
manages, acts on the workspace of its directory, without scope or lifecycle
tracking. `shoal list` shows each workspace's `path`. Leases need a workspace
Shoal manages.

Every space has a number, which `shoal list` and `shoal status` show. Select a
space by its number or its ID, as in `shoal attach 3` or `--space 3`. A name
never selects a space. A removed workspace has no number, so select it by its
ID. Approval requests and threads are numbered `S.N`: the number of their
space, then their own, such as `3.2`.

A chat in a workspace's worktree (`shoal start --in <workspace> -m chat`)
reads that workspace and changes nothing there. `shoal diff`,
`shoal conflicts`, `shoal status` and `shoal branch base` act on the
workspace. Commands that also act on chats act on the chat. `--space
<workspace>` reads the workspace's issue, PRs and CI runs. Removing the
workspace removes the chat.

## Start agents

```sh
shoal repo ls
shoal start https://github.com/owner/repo/issues/34 --agent claude
shoal start my-project --branch fix-login --agent codex
shoal start my-project --branch fix-ui --base fix-login --agent claude
shoal start my-project --mode plan --issue 42 --agent claude
shoal start --issue 43 --agent claude
shoal start my-project --branch api-part --agent codex -- "Move the API client into its own crate"
shoal start --in 3 --agent claude
shoal contribute https://github.com/other/repo/issues/12 --agent claude
```

`shoal start` takes a registered repository and a branch, or the URL of an
issue, PR, CI run or branch. `--pr` takes a PR's number or URL, and `--ci` a
run's. The agent's first message is the issue, else the run, followed by its
mode's instructions. Text after `--` is the task. A run starts in the
workspace of its PR or branch. A run on the default branch or a tag starts in
a chat named after it. The space links the run as related.

Every start starts an agent: the one `--agent` names, else `agent.default`,
else one picked on a terminal. Without any of these, `start` is refused. To
make a workspace without an agent, use `shoal create`.

The mode decides the kind of space. `--mode` picks the mode, else the mode
whose `label` the issue or PR carries, else `worker`. `worker` starts in a
workspace; `chat`, `plan` and `coordinate` start in a chat. A start that names no branch,
issue, PR or copy begins as a chat in its mode. `shoal convert workspace`
makes it a workspace once the work is known.

`--name` names the new space. Without it, the space takes the title of its
issue or PR, else its branch. `--base` with another workspace's branch stacks
the new workspace on that branch.

`--in <workspace>` starts that workspace's own agent instead of a new space.
Its task is the text after `--`, else its issue. With a chat mode, such as
`-m chat`, it starts a chat in the worktree instead. A workspace has one agent
of its own, so starting another is refused while one runs, waits in the queue
or is stopped there. `--replace` discards a stopped or asleep agent and starts
the new one in its place.

`shoal contribute` takes the same URLs, for a repository the user does not
maintain. It clones the repository and forks it when the user cannot push
there. It removes the clone once nothing uses it.

### Where a new space goes

Inside a space, `start` creates a child of that space. `--parent` names
another space in your scope as the parent. The child is in the same
repository, on a new branch from the default branch. A master session creates
children in any registered repository. Outside any space, `start` creates a
top-level space.

Agents start detached from inside a space, and wherever there is no terminal,
such as in a tool call. A chat-mode start returns once the chat is recorded,
with its agent's queue entry; the agent queue opens the chat in a background
place. `--here` runs the agent in the calling terminal, once its workspace is
prepared, until the agent exits.

`--fork` starts the child's agent from a fork of your session, so give it only
its task after `--`. With the same model, the child reads your prompt cache
instead of writing the conversation again.

`--machine <name or ID>` starts the space on another machine of the hub,
below the caller there too. Shoal carries over what the start needs, such as
a forked session and copied files.

### Preparation and the agent queue

`start` returns once the daemon records the new workspace, with the agent's
queue entry. The entry waits while the daemon creates the worktree and runs
setup. Until the worktree exists, commands that read it refuse the workspace.
`--wait` returns once preparation ends.

When preparation fails, the agent of the nearest space above with an agent
hears of it, or else the user.
`shoal setup <space>` retries it, and `shoal rm <space>` removes the space.
When an agent fails to start, `shoal queue` and `shoal inbox` show its entry
as `failed`, with its error. `shoal resume <space>` queues it again.

The daemon starts queued agents of heavy modes as `max_heavy_agents` allows,
higher priority first. `shoal queue` lists the entries with their `position`
and the `rank` that explains it, and `--now` starts one regardless.
`shoal priority <low|normal|urgent|0-100> [space]` and `start --priority` set a
space's priority, at most your own space's.

`--add-blocker <space|queued agent|issue>`, repeatable, holds the agent until
that work is done. A bare number is an issue. Select a space by its ID and a
queued agent by its entry's ID. An issue's agent also waits for the issues its
forge records as blocking it. The new workspace waits as `preparing`, then
starts on the done workspace's branch. `blockers` lists what still blocks the
agent.

## Child workspaces

```sh
shoal create --branch fix-typo           # New branch from the default branch
shoal create --issue 42                  # Named after the issue, which it links
shoal create --branch api-part --base <branch>  # Stacked on the given branch
shoal create --no-branch [--base <ref>]  # A workspace at a commit, no branch
shoal create --copy                      # A copy of this workspace's files
```

`shoal create` creates a workspace without an agent. It takes what
`shoal start` takes, such as a branch, an issue, or the URL of a PR, branch or
CI run. The workspace is a child of the current space, a workspace or a chat,
or of the space `--parent` names. It has its own worktree, branch and setup,
and the result gives its `path`. Shoal commands run in its directory act on
it. Its issue and PR activity reaches its parent's agent, each update naming
the child.

`--no-branch` makes a workspace at a commit, with no branch or PR. An agent
started there with `shoal start --in` cannot push.

`--copy` gives the child your committed, uncommitted, untracked and ignored
files. `--checkpoint <number>` gives it one of your checkpoints' files
instead. Cleanup removes a copy nobody changed. `shoal start --copy [--fork]
-- "<task>"` starts an agent in such a copy. `shoal start --copy <workspace>`
copies another workspace, live or removed.

`shoal rm <child>` removes a child, including one that a create left
`preparing`. Removing one the user holds, or one with such a space below it,
follows your `user_holds` permission.

## Convert a space

```sh
shoal convert workspace fix-login     # This chat becomes a workspace on fix-login
shoal convert workspace --issue 42    # On the branch the issue names, which it links
shoal convert chat                    # This workspace becomes a chat in the checkout
shoal convert agent codex -- "<note>" # Codex takes this space's work over
```

`convert` keeps the space with its number, name, parent, children, links,
threads, files and progress. An agent converts its own space and the spaces
below it.

`convert workspace` takes a branch, `--issue` and `--base`. It uses an
existing local or remote branch of that name. Otherwise it creates the branch
from the default branch, or from `--base`. Without a branch, `--issue` or the
chat's linked issue names it. The command returns once the worktree is set up.

`convert chat` removes the worktree as `shoal rm` does, keeping a checkpoint
and the branch. It releases the workspace's ports, resources, simulators and
secrets. The user's holds refuse it, and so does a workspace that builds on
its branch.

Once the agent's turn ends, Shoal restarts it in the new place from a fork of
its session. The agent is told what the space is now and its mode's
instructions. `--mode` picks the mode; by default, `worker` for a workspace
and `chat` for a chat.

`convert agent <tool>` hands the space's work to another agent tool, once the
running agent's turn ends, or at once for a stopped or asleep agent. `-a <tool>` on `convert workspace` or
`convert chat` does the same along with the kind change. Shoal attaches the
space's sessions to it as `handoff` files. The new tool starts a new session
that is told where they are, then its mode's instructions and the text after
`--`. It keeps the space's mode unless `--mode` names another. `--model`,
`--effort` and `--permission` set what the new agent runs with.

## Coordinate spaces below

The agent of the nearest space above with an agent coordinates a space. It
receives the space's agent's alerts, threads, completion, ready marks and
exits, as alerts that name the space; the user does not. It also receives the
issue and PR activity of a space without an agent of its own. An agent started
with `shoal start` receives its own issue and PR activity.

```sh
shoal send --space <child> "<text>"   # --body-file F, --attach FILE
shoal send <S.N> "<answer>"           # In a thread it holds you in
shoal stop <child>                  # --skip-children stops only the child
shoal stop --interrupt <child>      # Cancel its agent's turn, keeping it running
shoal restart <child>               # Its agent, a workspace's or a chat's
shoal resume <child>                # Bring back its agent, a workspace's or a chat's
shoal restore --discard-agent <child>  # Discard its stopped agent instead
shoal files <child>
shoal absorb <child> [--into <space>] [-X squash] [--force]
```

These commands act on the agent of a workspace or a chat alike:

- `stop` stops the child's agent and the agents below it, keeping their
  sessions. It cancels agents queued to start there.
- `stop --interrupt` cancels the agent's current turn. The agent keeps
  running.
- `restart` stops the agent and starts its session again at once.
  `--wait` restarts it once its turn ends, and `restart --cancel` drops that.
- `resume` continues a turn that an interrupt ended. It queues stopped agents,
  whoever stopped them, and launches that failed. It refuses a workspace whose
  setup failed and names `shoal setup`, which retries setup first.
- `restore --discard-agent` discards a stopped agent with its last sessions,
  stopped commands and failed launch. The files stay.

`--all` acts on every space below yours on this machine, and `--machine
<name>` on those of another joined machine. `stop --yes` confirms that an
execution Shoal cannot prove ended did end. A space outside your scope is
refused.

`absorb` takes a done child's work into your space, or into the space in your
scope that `--into` names, then removes the child:

- The child's branch merges into that workspace's branch.
- Its uncommitted files arrive as uncommitted changes, for you to review and
  commit.
- Its links, attached files, open threads and children move over. Its
  children move up one level.

`absorb` refuses a child that is not done, and changes nothing.
`--force` stops the child's agent and takes its work as it is. `absorb` also
refuses a merge that conflicts or touches your uncommitted files. A chat, or a
space of another repository, takes no code. It refuses a child whose code it
would lose, unless `--force` leaves that code behind.

### Decide the requests of spaces below

A child's approval request goes to its coordinator first.
`shoal approvals --approve <S.N>` approves a request to hold a workspace
below you. It approves a request that a [permission](#permitted-actions)
governs, such as a proposal, a push or a merge, only where your own value for
that permission is `direct`. `--title` and `--body` edit a proposal first. It
approves any other request only where you hold an approved grant for it, or
your `decide` permission is `direct`.

`--deny <S.N>` denies a request, with `--reason`. For an issue proposal,
`--duplicate-of` or `--duplicate-of-approval` denies it as a duplicate.
`--escalate <S.N>` passes a request to the space above or the user.

## Follow progress

```sh
shoal list            # Spaces, children below their parent; --chats for chats
shoal inbox           # Threads, approvals, waiting or failed spaces, ready marks
shoal status 3
shoal log 3           # Its log; --all for every event, --follow
shoal alert list      # Your alerts; --unread, --space S, --all
```

`status` shows a workspace's base, parent and children. `prs` lists its
watched PRs, each with its state, merge conflicts, CI results, review state
and lookup errors. `cleanup_reason` says why cleanup keeps the workspace.
`list` and `status` show the `agent_state` that agents' hooks report:
`working`, `waiting` or `idle`. Outside a space, `status` shows the daemon.

`log` lists what happened to a space, oldest first, such as its agent starts
and exits, links, PR changes, threads, approvals, completion and removal.
When a removal failed or stopped, the log says where. Select a removed space
by its ID. Outside a space, `log` lists every space's entries with the
daemon's.

On a machine joined to a hub, `list`, `inbox` and `status` also show other
machines' spaces in your scope, each with its `machine`. Commands reach those
spaces through the hub, except those that need the machine they run on, such
as leases, which name that machine. While it is offline, an alert, answer,
approval decision or stop waits until it connects, and other commands fail.

The user's alerts report agent exits, thread posts, busy resources, pending
approvals and cleanup Shoal did on its own. Reading them marks them read.
Scoped callers read them as their `notifications` permission allows.

## Space name

`shoal rename "<name>"` names the space's work, in free text at most 40
columns wide. Lists, and the tabs and windows Shoal opens for the space, show
the name. The branch and the worktree directory keep their own names.

## Progress

```sh
shoal progress "Reproduced; the cause is the query limit"  # "" clears it
shoal progress [--space S]          # Show the status line and steps
shoal progress --steps < plan.md    # `- [ ] name` / `- [x] name` per line
shoal progress --add-blocker 1468 --reason "CI runner is down" [--step 3]
shoal progress --remove-blocker 1468 [--step 3] [--space S]
```

`progress` sets the space's status line: one line, in the agent's words, that
`list` and `status` show. Claude Code's and Codex's plan tools set the steps
through Shoal's hooks; other agents use `--steps`. With `[pr] progress` on,
each open draft PR the space opened or was assigned ends its body with the
status line and steps, until it leaves draft. Progress changes notify nobody;
a coordinator reads its children's progress when it needs it.

`--add-blocker` records what blocks the space, or with `--step` one step, as
`shoal progress` numbers them. A blocker is an issue by number or URL, a PR or
CI run by URL, or a space by its ID. Give it a one-line `--reason`. A step's
blocker goes away with the step when the plan drops it.

A space is blocked while it has a blocker of its own, or while every step not
done has one. Lists show it as `blocked`, and its coordinator, or the user,
hears of it once. Once a blocked agent's turn ends, Shoal stops it. Shoal
restores it once nothing blocks the space, and its first message names what
cleared. A blocker clears when its issue closes, its PR merges or closes, its
space is done or removed, or its CI run passes. A CI run also clears when a
later run of its workflow passes on the default branch.

A coordinator removes a blocker of a space below with `--space`.
`shoal resume` restores a blocked agent at once. `shoal done` removes the
blockers.

## Sleep

```sh
shoal sleep                 # Asleep from the end of your turn until the next
shoal sleep --permanent     # Back to sleep after every turn until --undo
shoal sleep --for 20m -- "Check whether CI finished"   # Or --until 14:30
shoal sleep --undo [space]  # Awake, timer cancelled; an agent that stopped comes back
```

A mode can set `idle_wake_minutes`, which is 60 for `worker`. Its agent then
receives a message once it has stayed idle that long, and again after each
further period, until its space is done: "You have been idle for 60 minutes.
Continue your work if any remains; otherwise run `shoal sleep`." An agent with
nothing to do until something arrives runs `shoal sleep`. From the end of its
turn, it gets no idle wakes.

Shoal stops an idle agent that is not light seconds after its turn ends, and
keeps its session. When the user is engaged with the agent, Shoal waits
instead twice as long as the user usually takes to come back to it. The agent takes no queue slot until an update, a message,
an answer or the user wakes it.

`--for` or `--until` sets the space's timer, replacing any earlier one. When
it is due, the agent receives the text after `--` as a message, or a note that
the timer fired, asleep or not. The timer survives restores and daemon
restarts until `--undo` or `shoal done` cancels it. A loop sets its next timer
each turn. Shoal refuses Claude Code's own timer tools, since Shoal's stops
end their timers.

A coordinator puts a space below to sleep with `shoal sleep <space>`. `list`
and `status` show an agent that Shoal stopped on its own as asleep, with why
and what wakes it. An agent that `shoal stop` stopped, or that quit, stays
stopped until `shoal resume`.

## Issues and pull requests

```sh
shoal pr open [--title T] [--body-file F] [--label L] [--draft]
shoal pr edit --title T --add-label L --no-draft
shoal pr comment --body-file F [--attach FILE]
shoal pr reply <comment-id> --body-file F
shoal pr resolve <comment-id> --body-file F
shoal pr react +1 [--comment <comment-id>] [--remove]
shoal pr review [<number>] --comment --body-file F --line PATH:LINE 'text'
shoal issue open --title T --body-file F [--link] [--repo OWNER/NAME]
shoal issue ls <words> [--match keyword|meaning|both] [--state all] [--label L] [--author A] [--limit N]
shoal repo labels
```

These commands act on the linked item, or on the number or URL given. Shoal
acts as the agent account and records each change it makes. `issue` has the
same `edit`, `comment` and `close` commands as `pr`.

`pr open` pushes the branch, then links the branch's open PR or opens one. The
first assigned issue gives the default title. Without a body, the PR gets a
`Closes #<number>` line for each assigned issue; a given body is used as it
is. Opening refuses an item that lacks a label `required_labels` asks for.
`pr open`, `issue open` and `ready` report `warnings` for missing labels or
template headings. `repo labels` lists each label with its meaning.

`pr edit` also takes `--body-file`, `--add-reviewer`, `--add-assignee`,
`--milestone` and `--draft`; `shoal pr reviewers` lists who can review.
`pr comment --edit <comment-id>` changes a comment the agent wrote, and
`pr close` closes the PR. `open`, `edit` and `comment` take `--attach FILE`.
The body shows an image or video where it references the file as
`![alt](FILE)`, and links any other file there.

`shoal view` shows each review comment's ID. `reply` answers in that
comment's thread, and `resolve` replies and then resolves the thread.
`view --json` lists reactions.

`pr review` comments on lines of the diff. Pass `--line`, repeated, or
`--old-line` for a removed line. Or pass `--comments-file F` with a JSON array
of `{"path", "line", "body"}` objects, adding `"side": "old"` for a removed
line. A comment on a line the diff does not show refuses the whole review,
before anything is posted.

`issue open` records the issue as opened by the space; `--link` assigns it
instead. While an agent works an assigned issue, Shoal adds the agent account
to the issue's assignees on the forge, and removes it when the work stops.

`--repo OWNER/NAME`, or a repository URL on another forge, opens the issue in
another repository, as the `issue_other_repositories` permission allows. The
other issue and PR commands take `--repo`, or an item's URL, for any
repository, registered or not. An agent reads other repositories' items;
changing them is the user's job.

`issue ls` and `pr ls` match their words both by keyword and by meaning by
default, or by keyword alone on a forge that cannot search by meaning.
`--match` picks one way.

`shoal pr revert [<number>]` opens a PR that undoes a merged one. Where the
forge cannot, Shoal opens it from a child workspace already marked done,
which goes once the revert merges.

Some of these commands follow a [permitted action](#permitted-actions):

- `pr review --approve` and `--request-changes` follow `pr_review`. They act
  as the host's `review_token`, which must be set and must not be the account
  that opened the PR.
- `merge` of a PR, with `--auto` and `--cancel-auto`, follows `merge`. An
  approval binds the PR's head commit, the merge method and a squash's
  message.
- `ci approve` follows `ci_approve`.

### The forge

`shoal forge capabilities` lists what the forge supports, such as resolving
threads, rerunning jobs or reading job logs, and what to do instead.

`shoal forge api 'repos/{owner}/{repo}/<path>'` sends a GET to the forge's API
and prints the JSON; `--repo OWNER/NAME` names another repository. Other
methods go only to the space's own repository, as its `api_writes` permission
allows. `direct` sends them, `propose` asks for an approval bound to the
method, path and body, and `deny` refuses them.

`shoal forge download <url> --output <file>` saves an image or attached file
from the forge, with the same login.

## Permissions

A space's permissions come from its agent's mode in config. Each is no wider
than the same permission of the space above. Each key that a `--permission`
given to the agent sets then takes that value. `shoal status` lists the
space's permissions, each with where it comes from. A command that acts on a
space below uses your own permissions.

`shoal start --permission KEY=VALUE`, repeatable, gives a child a permission
whatever its mode allows, such as `--permission push=propose`. You give only
values your own space holds; a wider value is refused. `shoal resume` and
`shoal restart` take `--permission` too. It sets the keys it names and keeps
the rest, and `KEY=mode` returns a key to its mode's value. Without
`--permission`, they keep every key.

### Proposals and pushes

When the space's permissions require approval for issues and PRs,
`issue open` and `pr open` exit 0 with `proposed`, an approval request,
instead of the item. `pr open` still pushes, unless pushes need approval too.

The user approves a proposal, or a space above whose own permission opens the
item directly. Either may edit its title and description first. The decision
arrives as a message. It names the opened item, or for a denial the reason or
the duplicate issue. A proposed issue stays after the space is removed. A
pending PR proposal keeps the workspace from cleanup. A new commit outdates it
until `pr open` runs again. A chat proposes issues, never PRs.

When the `push` permission requires approval, `pr open` pushes nothing. It
proposes the PR together with the push, or only the push for an open PR. Plain
`git push` reports the push as rejected and `waits for approval <S.N>`;
`--force-with-lease` works the same way. On approval, Shoal pushes the
approved commit and opens the PR that waited for it. A new commit outdates the
request until the next push or `pr open`. A pending push keeps the workspace.
With `push = "deny"`, `pr open` and `git push` fail with the reason.

### The gate

Under a permission of `gate`, an agent called the gate decides the request at
once, instead of the user. An allowed request goes ahead as an approved one
does. A denial arrives as a message with the reason. Running the command again
asks again, until three denials of the same action refuse it. A request the
gate passes on goes to the space above or the user.

### Permitted actions

Other keys govern administering Shoal and actions that reach beyond a space.
`shoal config reference` says what each key governs and its default. These
are the ones agents meet most:

- `admin`: administering Shoal, such as `config set`, `repo add`, `cleanup`,
  `setup --skip`, `contribute` and restoring removed workspaces. You set a
  permission in config only to a value your space holds.
- `limits`: starting agents regardless of the limits, with `--now`,
  `--focus` or `queue start`.
- `user_holds`: releasing the user's holds and removing what the user holds.
- `daemon`: the daemon's service commands, `shoal update` and
  `shoal hub update`.
- `notifications` and `decide`, each `deny` or `direct`: reading the user's
  alerts, and deciding the requests of the spaces below as the user does.

With `propose`, the command exits 2 with `approval_pending` and the request.
The request goes to the space above whose own value is `direct`, or to the
user. Once it is approved, the same command proceeds once. A master session
holds every key at `direct`, except `daemon`, `triggers` and `api_writes`,
which it proposes. Other modes propose `admin`, `limits`, `ci_approve`,
`pr_review`, `triggers` and `user_holds`, and are denied the rest.

### Approval requests

```sh
shoal approvals
shoal approvals --approve <S.N>
shoal approvals --deny <S.N>
shoal approvals withdraw <S.N>
```

A command that needs approval returns `approval_pending` with the request
number, and `approval_denied` once it is denied. Run it again after approval
to proceed. `--reason` states the purpose of protected access. A request goes
to the nearest space above with an agent, then up to the user. Unscoped
callers decide any request. Approving a resource reserves no capacity.

## Links and watches

```sh
shoal link pr <number-or-url> assigned
shoal link ci <run-url> related
shoal unlink pr [<number-or-url>] assigned
shoal view [pr <number> | <url>] [--no-comments] [--check '<name>']
shoal watch pr [--timeout <seconds>]
```

A link names its relation last:

- `assigned`: the space's own work.
- `opened`: an item the space created another way.
- `related`: an item the work relates to.

Each item has one relation. Assigning an item replaces its opened or related
link. An assigned item refuses another relation until it is unlinked.
`unlink` without a number unlinks every assigned item of the kind.

A space can have several assigned issues. The first names a new branch and
gives a new PR its title. Commands that act on the assigned issue need its
number once there are several.

A workspace can have several assigned PRs, whatever their head branch, such
as a stacked PR pushed from the worktree. After `done`, cleanup waits until
all of them have merged and contain HEAD, or the default branch has their
changes. A PR closed without merging keeps it waiting until it is unlinked.

`shoal view` shows every linked item with its relation, description,
comments and reviews, inline comments included. For failed CI jobs, it shows
their URLs and, where the forge reads job logs, their log tails. `--check`
selects one check's log.

### Activity of watched items

Shoal reports the activity of watched items: comments and reviews, edits,
CI results, merge conflicts, closing and merging. A failed check comes with
its failing step, and passed checks once none on the PR's head is pending.
Shoal leaves out what the space itself did through Shoal, and the assignees
it adds or removes on its own. Activity from before
an item was linked, or before a new agent session started, is not reported;
`shoal view` shows it.

Where `[pr] watch_merge_runs` is on, the space's own merge into the default
branch also links that commit's CI runs on the default branch as related. Its
own merge is the merge of a PR linked as assigned or opened, or a local
`shoal merge` that pushes its branch. Shoal reports
their ends, and they keep a done space until they finish.

Hooks deliver activity to Claude Code, and to a workspace's Codex in a place
its host types into, when Shoal launched them. The activity arrives as context
after tool calls, and starts the next turn after a turn ends. A delivery
replaces a running watch.

Other agents run `shoal watch pr`, which returns `updates`. Without
`--timeout` (1–3600 seconds) the wait has no limit. `timed_out` with empty
`updates` means no update. A newer wait supersedes a running one, which
returns `superseded: true`. A wait for an item the space does not follow
includes the item's existing activity, and waits share a cursor per
workspace. A `lookup_failed` entry names a lookup that
failed. A `base_merged` update means the base workspace merged. It says
whether Shoal retargeted the PR and gives the rebase command.

## CI

```sh
shoal ci ls [--pr N]
shoal view ci <run-url> [--job '<name or ID>']
shoal ci rerun [--check '<name>'] [--pr N]
shoal ci rerun <run-url> [--failed]
shoal ci cancel [--check '<name>' | <run-url>]
shoal ci run <workflow> [--input NAME=VALUE] [--branch B]
```

`view ci` lists a run's jobs with the log tails of the failed ones. `--job`
prints one job's whole log, by its name or listed ID, whatever its result.

`ci rerun --check` reruns a failed check on the linked PR's current head.
Without `--check`, it reruns every failed check. `ci cancel` cancels running
and waiting checks. With a run URL, both act on that one run.

`ci run` starts a workflow, by its file name or its link. It runs on the
space's branch, on the default branch for a chat, or on `--branch`. This is a
new run, not a rerun: on Forgejo, a failed check on the PR stays failed. When
`ci run` links the run as opened, a watch reports when it finishes.

A space's agent acts on the runs of the default branch, of the branches of the
workspaces below it, and of their PRs on any commit. `--repo`, or a run's URL,
names another repository; anyone reads its runs, and only the user links and
acts on them.

A PR from a fork or a first-time contributor can show runs that wait for
approval instead of checks. `shoal view` lists them in `waiting_runs`.

## Local reviews

```sh
shoal review run                    # Or a workspace's number or ID, or a PR URL
shoal review ls                     # Reviews, newest first, with unresolved findings
shoal view review [<id>]
shoal watch review [<id>]
shoal review cancel [<id>]
shoal thread switch 3.1             # Your changes from now on fix finding 3.1
shoal send 3.6 "Only on empty input?"  # The reviewer answers in the thread
shoal thread resolve 3.1 fixed
shoal thread resolve 3.2 false-positive --reason "The caller checks the length"
shoal thread resolve 3.3 wont-fix --reason "..."
shoal thread resolve 3.4 deferred --issue 1240
shoal thread resolve 3.5 duplicate --of 3.2
```

`review run` queues a one-shot, read-only review of the committed HEAD.
Uncommitted edits stay out of it. With a PR URL, or from a chat for its
linked PR, it reviews the PR in the workspace that owns the PR's branch. The
reviewer writes its review with `thread open`, `send` and `review submit`,
under a scope that writes only that review. A run that does not submit fails.

Each finding is a `review` [thread](#threads), held by the workspace and
numbered in its space. Its opening post points at the reviewed lines. Once
the review is submitted, its summary and all its threads reach the
workspace's agent in one message, and its coordinator as one line.

The holder resolves a finding's thread with `thread resolve` and an outcome,
or posts in it with `send`. The reviewer answers there, from the workspace's
latest commit. Later reviews of the repository receive each `false-positive`
finding's title, path and reason, and skip that claim while the reason holds.
The next review checks each `fixed` finding against the changes made for its
thread. When the problem remains, it reopens the thread with a post saying
why.

A repository can require a review. Then `pr open` of a new PR, `ready` and
`merge`, of a PR or a branch, are refused until a finished review covers the
commit they act on. Every thread that review opened or reopened must also be
resolved. When no review covers the commit, the refusal starts a review of
the workspace's HEAD where that is the commit. Otherwise it names
`shoal review run <pr-url>`.

### Your own review of a space below

```sh
shoal review start <space>          # A pending review of a workspace below you
shoal thread open "Retries are unbounded" --space <space> --code src/fetch.rs:40-52 \
  --severity should-fix --body "Cap them at the configured limit."
shoal review submit [--approve] [--body T | --body-file F] [-s space]
shoal review cancel [<id>]          # Drop a pending review with its comments
```

You, or the user, review a workspace's work as the reviewer does. While your
review of a workspace is pending, a thread you open towards it without
`--kind` is a comment of that review. A comment is a `review` thread that only
your side and the user read, and its posts reach nobody yet. An explicit
`--kind` opens a thread outside the review. `--severity
blocker|should-fix|nit` says how much a comment matters, which the review's
tallies count.

`review submit` records the workspace's HEAD. It delivers the summary and
every comment to the workspace's agent in one message. A comment's opener
resolves it, and the holder's posts reach the opener. The review gates count a
submitted review once every comment is resolved. They count one without
comments only with `--approve`.

## Ready marks

A ready mark says that a space's agent needs the user before it can go on.
Its work is ready for review, or it has a decision or a question for them.

`shoal ready` marks the linked items, or the space when nothing is linked,
for the user's integrations. It takes a linked draft PR out of draft, unless
`[pr] undraft_on_ready = false`. It skips a linked PR that is not open, or
whose head on the forge is not HEAD, and names each on stderr with its reason.
New commits leave a mark as it was. `shoal ready --undo` withdraws the marks,
and `--draft` also returns the PR to draft.

## Alerts

```sh
shoal send "<text>" [--attach FILE]   # Up: to your coordinator, else the user
shoal send --space <child> "<text>"   # Down: to the agent of a space below
shoal send --in-chat [--space S] [--attach FILE]  # Your turn's final message
shoal alert list [--unread]           # Your space's alerts
shoal alert withdraw <number>         # Take back one you sent, until it is read
```

An alert is a one-way notice. It tells someone something without asking, and
affects neither completion nor cleanup.

Without `--space`, `shoal send` goes up to your coordinator, or else the
user, as one line. `--space`, `--all` and `--repo` send it down to the agents
below. `shoal send` prints each alert's number. Its sender withdraws it by
that number until it is read. `--space` names the space it went to, which may
be on another machine.

The user, Shoal and your coordinator send your space alerts, such as a request
to free memory or disk space. Hooks deliver them with PR activity. A new alert
also ends `shoal watch pr` with an `alerts` array, and `shoal alert list`
shows them. Each alert is shown once, with an `Attached file:` line for each
file attached to it.

`--in-chat` sends your turn's final message as the alert once the turn ends,
for an agent with Shoal's hooks. It names whom the alert goes to, and
`shoal log` shows the alert's number once it is sent. A prompt the user
submits during the turn drops the alert. Shoal tells you when your final
message goes unsent.

## Threads

```sh
shoal thread open "<name>" [--space <space>] [--kind question|request] [--code PATH[:FIRST-LAST]] \
  [--body T | --body-file F] [--severity blocker|should-fix|nit]
shoal send <thread> "<text>" [--choice A --choice B --recommend A --reason R | --ui FILE] \
  [--attach FILE] [--code PATH[:FIRST-LAST]] [--changes 12,14] [--body-file F] [--wait]
shoal send <thread> --action <ID> [--value KEY=VALUE]  # Answer the other side's surface
shoal send <thread> --in-chat        # Your turn's final message is the post
shoal thread forward <thread> [--space <space>] [--note "<text>"] [--unlink]
shoal thread resolve <thread> answered   # A request: done; any: duplicate --of, withdrawn --reason
shoal thread resolve <thread> --undo
shoal thread list [--space <space>] [--state open|resolved|all]
shoal view thread <thread>
shoal link thread <S.N> related
shoal unlink thread <S.N>
```

A thread is a conversation between its opener and its holder, the side asked
now. Both sides post in it. A `question` asks for an answer. A `request` asks
the holder to do something, such as implement the code it points at.

`thread open` opens a thread with your coordinator, or else the user. With
`--space`, it opens one with a space below you. From the user's terminal, it
opens one with a space. With `--body` or `--code`, it posts at once: the
opening post is the body, else the thread's name, with the references.
Nobody hears of an empty thread.

Each post reaches the other side as an alert, which wakes an idle agent, and
reaches the user as one line. Inside a space, a bare number names a thread of
that space; give others their full `S.N`.

You post on the side you took part on, such as the opener's or an earlier
holder's, and your post reaches that side's current party too. Any other space
that reads the thread posts as a participant. A participant's post reaches
both sides and takes no turn. A participant neither offers nor answers a
surface, and hears the replies from then on.

A post states its text, in Markdown. It may offer a surface, an interface the
other side picks or fills in. `--choice`, repeated, offers one button per
choice, numbered from 1, with the one it recommends and why. `--ui FILE` offers
any A2UI 1.0 `createSurface` message of the basic catalog's components; its
images name files sent with `--attach`. A post shows its surface as text. The
text ends with one line per action: its id, label, the `--value` keys it takes
and the command that answers it. `--action ID` answers the other side's latest
surface with that action, and `--value KEY=VALUE` gives each value it takes.
Shoal never reads text as an action. `--wait` waits, without a time limit,
for the next post from another side and prints it. `--in-chat` makes your
turn's final message the post, for an agent with Shoal's hooks. When the user
submits a prompt in your chat, that prompt answers it. A turn has one such
mark, for a post or an alert; another `--in-chat` replaces it and names the
one it replaced.

`--code`, repeatable, points a post at a file, or some of its lines. The file
is in the worktree of a space in your scope, or of the workspace your chat
runs in. `--attach` sends a copy instead. The other side receives each
reference with the lines' snippet. Shoal follows the code as it changes:
`view thread` shows each reference where its lines are now, or marks it
`changed` or `outdated`.

The opener or the holder forwards its role in a thread, down to a space it
reaches, or up to its coordinator or the user, never to the other side. The
new party reads what came before and receives the posts from then on. You
receive them too, unless `--unlink`. A new opener numbers the thread in its
own space.

The opener resolves its thread:

- `answered` for a question;
- `done` for a request, once it accepts the work;
- `duplicate --of <thread>`;
- `withdrawn`, which takes back the holder's unread alerts of the thread.

A space reads the threads it, or a space below it, took part in.
`link thread` delivers a thread's new posts to your space. A thread that waits
for its holder keeps the space that opened it from cleanup. The user's
waiting threads fill the inbox. `shoal done`, and removal, close the threads
the space opened or holds, and tell the other side. `done --undo` and a
restore reopen them.

## Changes

```sh
shoal thread switch <thread>         # Your changes from now on are for it; --none for none
shoal changes [--thread <thread>]    # Your latest changes, numbered, with their threads
shoal changes claim 12 14 [--thread <thread>]
shoal changes unclaim 11
shoal thread add <thread> 12 14      # Into that thread, out of any other
shoal thread remove <thread> 12
shoal diff --thread <thread> [--since <post>]
```

Shoal records what your tools change in your worktree. Each hunk is a change,
numbered in your space's own sequence. An edit tool's hunks are yours. A
command's hunks are yours, unless the user edited the same file while it ran,
which makes them `mixed`. What changed between your tool calls is someone
else's. A generated, binary or large file is one change without hunks.

`thread switch` sends your changes, and mixed ones, to an open thread your
space opened or holds, of any kind. That lasts until the next switch, or until
the thread is resolved. Editors show the thread as `working`.

After a command, Shoal lists the mixed changes it left. `claim` makes changes
yours, with `--thread` for that thread. `unclaim` makes them someone else's,
for no thread.

`view thread` shows the changes made for a thread between its posts. A post's
alert sums up the changes since the sender's last post. `diff --thread`
prints them, and `--since` those since a post, as `view thread` numbers them.
`send --changes` points a post at changes, as `--code` points at code.

## Files

```sh
shoal files [space]
shoal files add notes.md ['chart.html#Latency by region'] [--space S]
shoal files rm [FILE...] [--space S]
shoal done --attach report.md
```

`files add` copies files of at most 100 MiB into the space's attached files,
without telling its agents. `PATH#TITLE` gives a file a title. `files` lists
the attached files, and `rm` deletes copies. A removed space's files outlive
it for the retention period, and `shoal restore` gives them back. The
coordinator, or else the user, reads them with `shoal files <space>`.

## Completion

```sh
shoal done [space] [--summary TEXT] [--attach FILE]... [--keep-demos]
shoal done --integrated <workspace>
shoal done --undo [space]
```

`shoal done` records that the space's work is finished at its HEAD. Only
`done` records completion; a closed issue or a merged PR does not. `done`
notifies the coordinator, or else the user. It closes the space's open
threads, and stops its demos unless `--keep-demos`. `--summary` ends the
notice with one line, of at most 512 bytes, saying what the work found; it
becomes the status line. `--attach` hands back anything longer.

With cleanup on, Shoal removes the space and the spaces below it after the
grace period (`[cleanup] grace_minutes`), stopping their agents. Cleanup is on
unless `[cleanup.done] enabled = false`, and `--cleanup` overrides it.

- A done workspace goes once every assigned PR has merged and contains HEAD.
- Without a PR, it goes once its files are clean and every commit is pushed,
  on the default branch, or on the branch it builds on.
- A done chat goes whatever PRs it watches.

Holds, threads it opened that wait, and pending proposals or pushes keep a
space. Shoal keeps a checkpoint and the branch of each removed space, except a
disposable one, and `shoal restore` reopens it. `completion.error` and
`pr_cleanup.error` report cleanup or lookup failures.

`--integrated <workspace>` records that another branch has that workspace's
work under new commits. Until its HEAD moves, its unpushed commits no longer
keep it. `--undo` withdraws a recorded completion and reopens the threads it
closed. Links and watches stay, and cleanup goes on as if `done` had not run.

Where `[cleanup.merged]` is on, merged cleanup removes a merged workspace
nobody has used for hours, whether or not `done` ran.

```sh
shoal acquire hold --name <name> --reason <why> [--space <space>]
shoal leases hold
shoal release hold --name <name>
```

A hold keeps a space from cleanup. An agent asking to hold its own space
makes an approval request to its coordinator, or the user. Holding a space
below it needs no approval. Scoped callers release the holds agents placed,
and the user's as their `user_holds` permission allows.

## Remove, restore and move

```sh
shoal rm <space>
shoal checkpoint --name "<what comes next>"
shoal restore <id>
shoal restore [workspace] --checkpoint [<number>]
shoal cleanup spaces
shoal cleanup --dry-run
shoal cleanup caches
shoal move <space> --machine <machine> --yes
shoal move <space> --ssh <host> --yes
shoal move <space> -o - --yes | ssh <host> shoal restore --from -
```

Removing a space first removes the spaces below it, held ones included. It
asks what to do with unmerged work. `rm` returns once its checks pass and
removes the space in the background. A later failure arrives as a message, and
`--wait` waits for the removal.

A checkpoint keeps HEAD and unsaved files. Shoal also takes one when an agent
exits or is stopped. `restore --checkpoint` lists a workspace's checkpoints.
With a number, it returns to that checkpoint, after keeping the current state
in a new one. It refuses while another command runs in the workspace.

`restore <id>` reopens a removed workspace or chat below the space it was
below, and resumes its agents; `--no-resume` leaves them for `shoal resume`.
An agent restores a space whose parent its scope reaches. When a space's
`intermittent` machine is offline, `restore <space>` takes the space over
here, from the copy the hub keeps, as the space's `admin` permission allows.

`cleanup spaces` removes every space that automatic cleanup would remove,
without waiting for its idle delay. `--dry-run` lists those, and says why each
other space stays.

`move --machine` moves a space to another machine joined to the same hub,
under its ID and number; `--recursive` moves the spaces below it too.
`move --ssh` moves a space, with the spaces below it, to a machine `ssh`
reaches. Both resume its agents there unless `--no-resume`. With `--keep`,
they copy the spaces instead. An agent's move of its own space starts once its
turn ends, and the agent continues there. `restore --from` opens the spaces
that `shoal move` wrote to a file.

`cleanup`, `restore --from`, `move --ssh` and `move -o` follow the space's
`admin` permission. `move --machine` needs none, except for a `--keep` copy of
your own space.

## Branches

```sh
shoal repo sync
shoal conflicts [<branch>]
shoal branch base [set <workspace>]
shoal branch ls [--merged | --unmerged] [-r REPO]
shoal branch delete <branch>... [--force] [-r REPO]
shoal refresh [<workspace>]
shoal merge [-s <space>]
shoal merge [pr] <number-or-url> [-X merge|squash|rebase] [--message M] [--auto] [--delete-branch]
shoal unmerge [-s <workspace>]
```

The daemon keeps the local default branch in sync with its remote, within
about a minute. `repo sync` syncs it at once, also in the registered checkout,
where `git fetch origin main:main` is refused. It never pushes, and never
changes a workspace's branch. Other workspaces' branches are shared refs; a
pushed one is current as `origin/<branch>` after a sync.

`shoal refresh [workspace]` brings a workspace that holds nothing of its own up
to date with what it was made from:

- a branch fast-forwards to its upstream or the branch it builds on;
- a workspace at a commit moves to its PR's head or its branch's tip;
- a copy takes its live source's files again.

When the workspace has work of its own, `refresh` refuses and names what deals
with it.

`conflicts` checks committed HEAD against the base workspace's branch, else
the local default branch, or the branch named. It never touches the worktree.
Exit 1 lists the conflicting paths. It also lists, under "Changed on both
sides", a file both branches changed that the repository marks as holding a
number each change picks, such as a migration. Without a watched PR, the
watch reports a `merge_conflict` when the check newly fails, until the space
is done.

`branch base set` records that a workspace builds on another's branch.

`branch ls` lists the forge's branches of the space's repository, or of the
one `--repo` names. Each row says whether the default branch has the branch's
changes, which open PRs it heads, and which workspace it belongs to.

`branch delete` deletes branches on the forge, never local ones, as the
space's `push` permission allows. It deletes none while one of them is the
default or protected, in an open PR, or an active workspace's branch outside
your scope. Without `--force`, it also deletes none while one has changes the
default branch lacks.

`merge` merges a space's work. It merges the one open PR linked as assigned or
opened, on the forge. Without one, it merges a workspace's committed branch
into the default branch, locally. Several open PRs, or a chat without one,
refuse it; closing the PR lets a workspace merge locally. A number, a PR URL
or `pr` merges that PR, and `pr` alone the assigned one.

A PR merges by `-X`, else by `[merge] method`, else by the repository's
default. `--message` sets a squash merge's commit message. `--auto` merges the
PR once its checks pass, and `--cancel-auto` withdraws that. A local merge
takes `-X` and `--message` too, squashing or rebasing the branch onto the
default branch, and `--push` or `--no-push`. A flag of the other path is
refused. `unmerge` undoes a workspace's latest local merge while the default
branch is unpushed.

Unless a workspace process's `merge` permission is `direct`, its `merge` or
`unmerge` first exits 2 with `approval_pending` for the PR's or branch's
current head. After approval, the same command at the same head proceeds; a
new commit needs a new request. `-s` acts on a space below. A merge that
pushes asks for confirmation on a terminal, and `--yes` gives it.

## Ports

```sh
shoal catalog port
shoal leases port
shoal acquire port web [--port N] [--reason "purpose"]
shoal release port web
```

`catalog port` lists the configured names and defaults. Repeating a
reservation returns the same port. The returned `port` is the reserved one.
Exit 2 with `reserved: false` is a conflict and a suggestion, not a
reservation; repeat the request with `--port <suggested_port>` to reserve
that port. New reservations do not change the environment of running
commands.

Reservations and leases outlive the command that made them. An agent's
simulator leases, containers and resource permits end with the agent, when it
quits or is stopped, but not while it sleeps. The user's stay until released.

## Resource pools

```sh
shoal catalog resource
shoal leases resource
shoal acquire resource devices [--member M] [--name N] [--mode read|write] [--queue]
shoal release resource devices [--name N]
```

The result names the leased `resource`. Each semaphore lease takes one
permit. The same `--name` returns the same lease. Release without `--name`
frees every lease you hold in the pool. Use is cooperative.

A busy request exits 2. `--queue` waits, without a time limit, until the lease
is granted or its approval denied. Queued requests take capacity in order of
arrival, and stopping one gives up its place. While others are queued, a
request without `--queue` is busy.

An `rwlock` resource has shared readers and one exclusive writer. `--mode`
picks one, write by default. `read_available` and `write_available` give its
capacity. To change mode, release first.

## Simulators

```sh
shoal catalog sim
shoal leases sim
shoal acquire sim [--queue]
shoal acquire sim --clean --reason "Verify first-launch permission prompts"
shoal release sim
```

`catalog sim` lists the configured profiles and the installed runtimes.
`acquire sim` returns an exclusive lease on a device, with its `udid`. Use the
`udid`: simctl's `booted` selector can name another workspace's device. Exit
2 means capacity is busy.

Shoal manages its own devices, and normal reuse keeps their apps, data and
settings. `--clean` erases the device and records the reason in an audit
trail; it refuses while a lease is held.

## Containers

```sh
shoal catalog containers
shoal acquire containers [--reason "purpose"]
shoal leases containers
shoal release containers
```

A repository can name a Compose file in `[containers]`. Then
`acquire containers` reserves its named ports and starts the workspace's own
Compose project. It returns the services with the ports they published.
Repeating it returns the running project. Exit 2 means a port it reserves
waits for approval, or its preferred port is taken.

`release containers` stops the containers and keeps their volumes and ports.
Removing the workspace removes them with their volumes. With
`start = "setup"`, they start once setup readies the workspace, before its
post-setup hook and agent. An agent that starts there starts them when they
are stopped. Chats have no containers.

## Secrets

`shoal catalog secret` lists the configured secrets and which need approval.
A tool whose secret needs approval fails with the request to make:
`shoal acquire secret NAME --reason "purpose"`. The value never reaches the
agent's environment. `shoal release secret NAME` gives up the grant.

## Demos

```sh
shoal demo ls                      # Configured and running demos
shoal demo start                   # The repository's only configured demo
shoal demo start web               # A configured demo by name
shoal demo start tui -- ./target/debug/app   # Any command, named tui
shoal demo start site --preset web -- npm run dev
shoal demo screen tui              # What the demo shows now
shoal demo send tui $'q'           # Type into it; $'\r' is Enter
shoal demo stop tui
```

Shoal keeps a demo's command running and restarts it when it exits. It tells
the user how to open the demo, and `shoal demo` and the inbox open it for
them. Chats have no demos.

A preset prepares what the demo needs:

- `server` reserves a port and passes it as `PORT`.
- `web` does the same, for a page opened in a browser.
- `simulator` leases the workspace's simulator and passes its UDID as
  `SHOAL_SIM_UDID`.

`shoal demo NAME --workspace W` reaches a `server`, `web` or `simulator` demo
that another machine of the hub runs. It forwards the demo to a port of this
machine and prints where, until stopped. For a simulator demo, that is its
viewer: a page that shows the device and takes touches. A terminal demo there
opens in the calling terminal as `shoal attach` does, so it needs one.

## Triggers

```sh
shoal trigger ls [--repo R]   # Source, paused or active, next firing, last run and its space
shoal trigger pause <name>    # It fires, and launches a queued run, no more until resumed
shoal trigger resume <name>   # It counts its times from now; those it missed never run
shoal trigger run <name>      # Fire it now and print the run's outcome
shoal trigger run <name> --payload issue.json --header X-Forgejo-Event=issues
```

A trigger, `[triggers.NAME]` in a repository's config or the global config,
starts an agent when its source fires. A source is, for example, a timer at
set times or an HTTP request to the trigger's webhook route. The agent starts
as the user's `shoal start` would: through the agent queue, in a top-level
space, with its mode's permissions.

The commands act on the triggers of the repository you act in, or the one
`--repo` names, and the global config's. They change a trigger's runtime
state, never its config. `ls` is open to every space. `pause`, `resume` and
`run` follow the `triggers` [permission](#permitted-actions).

`run` prints the run's outcome, or the filter condition that dropped it.
`--payload` and `--header` give a webhook trigger the request to fire with,
without its signature.

## Processes

After a tool call, Shoal names the processes your space's commands left
running after their parent exited, such as a failed test run's servers.
`shoal stop --leftovers` stops them. `shoal stop --processes` stops every
process of the workspace that Shoal does not track, never the agent or its
shells. `shoal setup` reruns the configured setup command and post-setup hook.

## Daemon

Installing, starting, stopping and restarting the daemon, `shoal update`, and
`shoal hub update`, which installs the latest release on the hub, follow the
space's `daemon` [permission](#permitted-actions). Stopping the daemon stops
every agent.

While a stop or restart waits for the daemon's own work, commands that start
more work, such as `shoal rm`, wait. They run on the next daemon after a
restart, and fail after a stop. Agents' other `shoal` commands fail while the
daemon restarts.
`shoal config reload` applies the config on disk without stopping anything.
An invalid file leaves the previous settings in place.

`shoal pause` stops all work on the machine until `shoal unpause`, even
across restarts. Every agent stops with its session saved, and spaces with
something to do continue on other machines of the hub. An alert names where
each space went. Only the user's own terminal pauses or unpauses a machine.

## Skill

`shoal skill` prints this skill. `shoal skill install` installs it into every
AI tool's existing skill directory. A tool name selects one tool and creates
its directory. It is refused in a scoped execution. Later unscoped commands
keep installed copies current.
