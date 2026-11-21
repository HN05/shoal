---
name: shoal
description: What Shoal's commands do, for agents in a Shoal workspace or chat and for sessions outside one — start and coordinate agents, open, change, watch and review issues and PRs, mark work ready, ask the user, hand back files, finish with `shoal done`, sync the default branch, create workspaces without an agent, and reserve ports, simulators, containers, resource permits and secrets.
---

# Shoal

Commands print JSON and never prompt when `SHOAL_OUTPUT=json`, which Shoal sets
for the agents it launches; otherwise `--json` on each command does the same.

## Spaces and scope

A space is a workspace, with its own worktree and branch, or a chat, with none.
Spaces form a tree: each has at most one parent. Agents Shoal launches are
scoped to their own space and the spaces below it; only the user's own
terminal is unscoped. A master session, a chat without a repository that
`shoal start --mode orchestrate` starts in `root_dir`, has a scope that reaches
every space. Inside a space, commands without a target act on it, or
on a child workspace when run inside the child's directory; outside one, they
open a picker on a terminal. A session Shoal did not launch in a managed
worktree resolves the workspace from its directory, without scope or lifecycle
tracking; `shoal list` shows the workspaces with their `path`. Leases need a
managed workspace.

Every space has a number, which `shoal list` and `shoal status` show. A
command that takes a space selects it by that number or its ID:
`shoal attach 3`, `--space 3`. A name never selects a space. A removed
workspace has no number and is selected by its ID. Approval
requests and threads are numbered `S.N`: the number of the space they belong to,
then the item's own, such as `3.2`.

A chat in a workspace's worktree (`shoal start --in <workspace> -m chat`) reads that
workspace and changes nothing there: `shoal diff`, `shoal conflicts`,
`shoal status` and `shoal branch base` act on the workspace, the commands that
also act on chats act on the chat, and `--space <workspace>` reads the
workspace's issue, PRs and CI runs. Removing the workspace removes the chat.

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

`start` accepts a registered repository and branch, or an issue, PR or branch
URL, and `--pr` a PR's number or URL; an issue becomes the agent's first
message, followed by its mode's instructions, and text after `--` is the task. Every start starts an agent:
`--agent`, else `agent.default`, else one picked on a terminal; without
either it is refused, and `shoal create` makes a workspace without an agent.
`--mode` picks the mode, else
the one whose `label` the PR or issue carries, else `worker`, which starts in
a workspace; `chat` and `plan` start in a
chat; the mode alone decides the kind of space. `--name` names
the new space; without it, it takes the issue's or PR's title, else its
branch. `--base` naming another workspace's branch stacks the new workspace on
it. A start that names no branch, issue, PR or copy begins as
a chat in its mode, `worker` by default, which `shoal convert workspace` makes
a workspace once the work is known. `--in <workspace>` starts that existing
workspace's own agent instead of a new space, with the task after `--`, else on
its issue, or a chat in its worktree in a chat mode such as `-m chat`; a workspace
has one agent of its own, so starting another is refused while one runs,
waits in the queue or is stopped there; `--replace` discards a stopped or
asleep one and starts the new agent in its place. `contribute` takes
the same URLs for a repository the user does not maintain: it clones it, forks
it when the user cannot push there, and removes the clone once nothing of it is
left.

Run inside a space, `start` creates a child of it, or of a space its scope
reaches that `--parent` names, in its repository, or for a master session in any registered
one, on a branch from the default branch;
a chat-kind mode starts a child chat in a background place, which the agent
queue opens, and returns once the chat is recorded, with its agent's entry. Outside any space it creates top-level
spaces. Without a terminal, as from a tool call, and from inside a space, agents
start detached; `--here` runs one in the calling terminal, once its workspace
is prepared, until it exits, as the new space's own agent. `--fork` starts the caller's
own agent from a fork of its session, so it needs only its task after `--`.
`--machine <name or ID>` starts the space on another machine of the hub, below
the caller there too, where that machine's queue starts its agent in the
background; it clones the repository where that machine has none, carries
a `--base` with commits only this machine has there in a Git bundle, and
carries the session `--fork` forks and the files `--copy` copies, ignored
ones excepted.

`start` returns once the daemon records a new workspace, with the workspace
and the agent's queue entry, which waits while the daemon creates the
worktree and runs setup and the post-setup hook; until the worktree exists,
commands that read it refuse the workspace. A failed preparation reaches the
agent of the nearest space above with one, or else the user, naming
`shoal setup <space>`, which retries it, and `shoal rm <space>`. `--wait`
returns once that preparation ends. An agent whose start fails, in a
workspace or a chat, stays on its space as a `failed` entry `shoal queue` and
`shoal inbox` list with its error, and its failure reaches the same way,
naming `shoal resume <space>`, which queues it again as it was, and
`shoal rm <space>`. The daemon starts queued agents of heavy modes as `max_heavy_agents` allows
(`state: queued`, `position`), work close to finished, work that other work
waits on and new workspaces whose issues share fewer `[issue] area_prefix`
labels with existing workspaces first (`rank`), higher priority before all
else; `shoal queue` lists
entries and `--now` starts one regardless. `shoal priority <low|normal|urgent|0-100> [space]`
and `start --priority` set a space's priority, at most your own space's. `--add-blocker <space|queued agent|issue>` (repeatable)
holds an agent until that work is done, where a bare number is an issue, a
space is selected by its ID and a queued agent by its entry's ID, and an
issue's agent is also blocked by the issues its forge records as blocking it;
the new workspace waits `preparing`, without its worktree, until that work is
done, then is created on the done workspace's branch, and `blockers` lists
what still blocks its agent. A Happy agent reports
its launch (`execution_id`, `pid`, `log`); a `happy-codex` launch fails when
its first message cannot reach the session, such as without a Happy login.

## Child workspaces

```sh
shoal create --branch fix-typo           # New branch from the default branch
shoal create --issue 42                  # Named after the issue, which it links
shoal create --branch api-part --base <branch>  # Stacked on the given branch
shoal create --no-branch [--base <ref>]  # A workspace at a commit, no branch
shoal create --copy                      # A copy of this workspace's files
```

`create` creates a workspace without an agent from what `shoal start` takes
for one, such as a branch, an issue or a PR or branch link, as a child of the
current space, a workspace or a chat, or of the one `--parent` names, with its
own worktree, branch and setup, and returns its `path`. Shoal commands run in the
child's directory act on the child. The child's issue and PR activity reaches
the parent's agent, each update naming the child. `--no-branch` has no branch
or PR of its own; an agent `shoal start --in` starts there cannot push. `--copy` gives the child the committed, uncommitted,
untracked and ignored files, and `--checkpoint <number>` one of the
checkpoints' files instead; an untouched copy goes with cleanup. `shoal start
--copy [--fork] -- "<task>"` starts an agent in such a copy, and
`shoal start --copy <workspace>` copies another workspace, live or removed.
`shoal rm <child>` removes a child, including one a create left `preparing`;
one the user holds, or with such a space below it, as the space's
`user_holds` permission allows.

## Convert a space

```sh
shoal convert workspace fix-login     # This chat becomes a workspace on fix-login
shoal convert workspace --issue 42    # On the branch the issue names, which it links
shoal convert chat                    # This workspace becomes a chat in the checkout
```

`convert` keeps the space, with its number, name, parent, children, links,
threads, files and progress. `convert workspace` takes a branch, `--issue`
and `--base`: an existing local or remote branch of that name, or a new one from
the default branch or `--base`; without a branch, `--issue` or the chat's
linked issue names it. It returns once the worktree is set up. `convert chat`
removes the worktree as `shoal rm` does, keeping a checkpoint and the branch,
and releases its ports, resources, simulators and secrets; the user's holds and
a workspace building on its branch refuse it. Once the agent's turn ends,
Shoal restarts it in the new place from a fork of its session, told what the
space is now and its mode's instructions; `--mode` picks that mode, `worker`
for a workspace and `chat` for a chat by default. An agent converts its own
space and the spaces below it.

## Coordinate spaces below

The agent of the nearest space above with an agent coordinates a space: its
agent's alerts, threads, completion, ready marks and exits arrive there as
alerts naming it, instead of reaching the user, and the issue and PR
activity of a space without an agent of its own arrives too. An agent started
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

`absorb` takes a done child's work into your space, or `--into` another your
scope reaches, and removes the child: its branch merges into that
workspace's branch, its uncommitted files arrive there as uncommitted changes
for you to review and commit, and its links, attached files, open threads
and children move over, the children up one level. It refuses, changing
nothing, a child that is not done, unless `--force` stops its agent and takes
its work as it is, and a merge that conflicts or touches your uncommitted
files. A chat, or a space of another repository, takes no code: a child with
code it would lose is refused unless `--force` leaves the code behind.

`stop` stops the child's agent and those below it, a workspace's or a chat's,
keeping their sessions, and cancels agents queued to start there; `restart` stops the child's agent and
starts its session again at once, or with `--wait` once its turn ends, which
`restart --cancel` drops; `stop --interrupt` cancels the current
turn of the child's agent, a workspace's or a chat's, which stays running;
`resume` continues a turn an interrupt ended and queues the stopped agents,
a workspace's or a chat's, whoever stopped them, and a launch that failed to
start; a workspace whose setup failed is refused, naming `shoal setup`, which
retries it before the agent waiting for it starts. `restore --discard-agent` discards a stopped agent,
its last sessions, stopped commands and failed launch, leaving the files. `--all`
acts on every space below the caller's on this machine, `restart --machine
<name>` on those of another joined machine, and `stop --yes` confirms that an
execution Shoal cannot prove ended did. A space outside the caller's scope is
refused with an error naming it.

A child's approval request comes to its coordinator first.
`shoal approvals --approve <S.N>` works for a request to hold a workspace
below; for an issue or PR proposal, a push, a merge, an unmerge or another
[permitted action](#permitted-actions) where the coordinator's own permission
for it is `direct`, taking `--title` and `--body`
for a proposal as the user's approval does; and for anything else only where
the coordinator holds an approved grant for it. Otherwise it is refused,
unless the coordinator's `decide` permission is `direct`, which decides any
request of a space below as the user does, routed to it or not, but approves
one a permission governs only where its own value for it is `direct`.
`--deny <S.N>` denies it, with `--reason`, or for an issue proposal
`--duplicate-of` or `--duplicate-of-approval`, and `--escalate <S.N>` passes it
to the space above or the user.

## Follow progress

```sh
shoal list            # Spaces, children below their parent; --chats for chats
shoal inbox           # Threads, approvals, waiting or failed spaces, ready marks
shoal status 3
shoal log 3           # Its log; --all for every event, --follow
shoal alert list      # Your alerts; --unread, --space S, --all
```

`status` names a workspace's base, parent and children, its watched PRs in
`prs` with state, merge conflicts, CI results, review state and lookup errors,
and in `cleanup_reason` why cleanup keeps it. `list` and `status` show the
`agent_state` agents' hooks report (`working`, `waiting`, `idle`). `log`
lists a space's creation, agent starts and exits, links, PR changes, ready
marks, completion, threads, approvals, alerts, child spaces, parent
changes, absorbs and its removal, with where a removal failed or stopped, oldest first; a removed
space is selected by its ID. Outside a space, `status` shows the daemon and
`log` every space's entries with the daemon's. On a machine joined to a hub,
`list`, `inbox` and `status` also show the other machines' spaces your scope
reaches, each with its `machine`. Commands that act on a space work on one
elsewhere through the hub, under your scope, except those that need the
machine they run on, such as `setup`, leases, starting a demo and attaching
files, which name its machine. While that machine is offline, an alert, answer, approval decision
or stop prints that it is queued and goes through once it connects; other
commands fail, naming the machine.
The user's alerts report agent exits, thread posts, busy resources, pending
approvals and cleanup Shoal did on its own; reading them marks them read, and
scoped callers read them as their `notifications` permission allows.

## Space name

`shoal rename "<name>"` names the space's work, in free text of at most 40
characters; lists and the tabs and windows Shoal opens for it show the name.
Its branch and worktree directory keep theirs.

## Progress

```sh
shoal progress "Reproduced; the cause is the query limit"  # "" clears it
shoal progress [--space S]          # Show the status line and steps
shoal progress --steps < plan.md    # `- [ ] name` / `- [x] name` per line
shoal progress --add-blocker 1468 --reason "CI runner is down" [--step 3]
shoal progress --remove-blocker 1468 [--step 3] [--space S]
```

`progress` sets the space's status line, one line in the agent's words that
`list` and `status` show. Claude Code's and Codex's plan tools set the steps
through Shoal's hooks; `--steps` sets them for other agents. With
`[pr] progress` on, each open draft PR the space opened or was assigned ends
its body with a section showing both until it leaves draft. Changes never
notify; a coordinator reads its children's progress on demand.

`--add-blocker` records what blocks the space, or with `--step` one step as
`shoal progress` numbers them: an issue by number or URL, a PR or CI run by
URL, or a space by its ID, with a one-line `--reason`. A step's blocker
goes with the step when the plan drops it. A space is blocked while it has a
blocker of its own or every step not done has one; lists show it `blocked`,
and its coordinator, or the user, hears of it once. Once a blocked agent's
turn ends, Shoal stops it and restores it when nothing blocks the space any
longer, its first message naming what cleared: an issue closing, a PR merging
or closing, a space done or removed, or a CI run, or a later run of its
workflow on the default branch, passing. A coordinator removes a blocker of a
space below with `--space`; `shoal resume` restores a blocked agent at once,
and `shoal done` removes the blockers.

## Sleep

```sh
shoal sleep                 # Asleep from the end of your turn until the next
shoal sleep --permanent     # Back to sleep after every turn until --undo
shoal sleep --for 20m -- "Check whether CI finished"   # Or --until 14:30
shoal sleep --undo [space]  # Awake, timer cancelled; an agent that stopped comes back
```

An agent whose mode sets `idle_wake_minutes`, 60 for `worker`, receives a
message once it has stayed idle that long, and again after each further
period, until its space is done: "You have been idle for 60 minutes. Continue
your work if any remains; otherwise run `shoal sleep`." An agent that has
nothing to do until something arrives runs `shoal sleep`: from the end of its
turn it gets no idle wakes. Shoal stops an idle agent that is not light,
keeping its session, slot-free until an update, a message, an answer or the user
wakes it, seconds after its turn ends unless the user is engaged with it, and
otherwise once it has waited twice as long as the user usually takes to come
back to it. `--for` or `--until` also sets the space's timer, which
replaces the one it had: when it is due, the agent receives the text after
`--` as a message, or that the timer fired, asleep or not, through restores
and daemon restarts, until `--undo` or `shoal done` cancels it. A loop sets
its next timer each turn; Claude Code's own timer tools are refused, since
Shoal's stops end their timers. A coordinator puts a space below to sleep
with `shoal sleep <space>`. An agent Shoal stopped on its own is asleep; `list` and
`status` show it with why and what wakes it. One stopped by `shoal stop`, or
that quit, stays stopped until `shoal resume`.

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

`pr open` pushes the branch, then links the branch's open PR or opens one. The
first assigned issue supplies the default title, and without a body each
assigned issue gets a `Closes #<number>` line;
a supplied body is used verbatim. Opening refuses an item without a label
`required_labels` asks for; `pr open`, `issue open` and `ready` report
`warnings` for missing labels or template headings. `repo labels` lists each
label with its meaning. Shoal acts as the agent account and records each
change it makes. `pr edit` also takes `--body-file`, `--add-reviewer`
(`shoal pr reviewers` lists who can review), `--add-assignee`, `--milestone`
and `--draft`; `pr comment --edit <comment-id>` changes a comment the agent
wrote; `pr close` closes; `issue` has the same commands. `open`, `edit` and
`comment` take `--attach FILE`, and the body shows images and videos and links
other files where it references them as `![alt](FILE)`. `shoal view` shows
each review comment's ID; `reply` answers in its thread, and `resolve` replies
and then resolves the thread. `view --json` lists reactions. `pr review` adds
comments on lines of the diff: `--line`, repeated, `--old-line` for a removed
line, or `--comments-file F` with a JSON array of `{"path", "line", "body"}`
objects, with `"side": "old"` for a removed line. A comment on a line the diff
does not show refuses the whole review before anything is posted. These
commands act on the linked item, or on the number or URL given.

`issue open` records the issue as opened by the space, or with `--link`
assigns it instead. An issue a space links as assigned has the agent account
among its assignees on the forge while an agent works it, from the link or the
agent's start until `shoal done`, a stop that waits for `shoal resume`, an
unlink or the space's removal, and linking it warns of anyone else assigned;
`[issue] assign` also or instead adds the user's own login. `--repo OWNER/NAME`, or a repository URL on another forge, opens it
in another repository, as the space's `issue_other_repositories` permission
allows;
`issue ls`, `pr ls`, `view` and the other issue and PR commands take `--repo`
too, or an item's URL, for any repository, registered or not: an agent reads
another repository's items, and changing them is the user's. `issue ls` and
`pr ls` match their words by
`--match keyword`, `meaning` where the forge searches by meaning, or `both`
ways by default, which on another forge is its keyword search. `shoal pr revert [<number>]`
opens a PR that undoes a merged one; where the forge cannot, it opens it from a
child workspace already marked done, which goes once the revert merges.
`pr review --approve` and `--request-changes` follow the space's
`pr_review` [permission](#permitted-actions), as the host's `review_token`,
which must be set and must not be the account that opened the PR. `merge` of
a PR, its `--auto` and `--cancel-auto` follow the space's
`merge` permission, a merge bound to the PR's head commit, its method and a
squash's message, and `ci approve`
its `ci_approve` [permission](#permitted-actions).

`shoal forge capabilities` lists what the forge supports, such as resolving
threads, rerunning jobs or reading job logs, and what to do instead.
`shoal forge api 'repos/{owner}/{repo}/<path>'` sends a GET to the forge's API
and prints the JSON, `--repo OWNER/NAME` for another repository; other methods
go to the space's own repository as its `api_writes` permission allows:
`direct` sends them, `propose` asks for approval bound to the method, path
and body, and `deny` refuses them.
`shoal forge download <url> --output <file>` saves an image or attached file
from the forge with the same login.

### Proposals and pushes

A space's permissions are its agent's mode's under config, each no wider
than the space above it, then each key a `--permission` given to its agent
sets; `shoal status` lists them, each with where it comes from. A command acting on a space below uses the
caller's own. `shoal start --permission KEY=VALUE`, repeatable, gives a child
a permission whatever its mode allows, such as `--permission push=propose`;
you give only what your own space holds, and a value above that is refused.
`shoal resume` and `shoal restart` take `--permission` too, which sets the
keys it names, `KEY=mode` dropping one, and keeps the rest; without it they
keep them all.

Where a space's permissions require approval for issues and PRs, `issue open`
and `pr open` exit 0 with `proposed`, an approval request, instead of the item;
`pr open` still pushes unless pushes need approval too. The user approves a
proposal, or a space above whose own permission opens it directly, and may
edit its title and description first; the decision arrives
as a message naming the opened item, or giving a reason or the duplicate issue
for a denial. A proposed issue stays after the space is removed. A pending PR
proposal keeps the workspace; a new commit outdates it until `pr open` runs
again. A chat proposes issues, never PRs.

With the space's `push` permission requiring approval, `pr open` pushes
nothing: it proposes the PR together with the push, or only the push for an
open PR. Plain `git push` reports the push as rejected and `waits for approval
<S.N>`; `--force-with-lease` works as usual. On approval Shoal pushes the
approved commit and opens the PR that waited for it. A new commit outdates the
request until the next push or `pr open`. A pending push keeps the workspace.
With `push = "deny"`, `pr open` and `git push` fail with the reason.

Under a permission of `gate`, the gate, an agent, decides such a request at
once instead of the user: an allowed one goes ahead as an approved one does,
and a denial arrives as a message with the reason. Running the command again
asks again; after three denials of the same action, a push of one commit, a PR
proposal with the same title, description and labels at one commit, or an
issue by its title, the command is refused until it changes. A request the gate passes on goes to the space
above or the user, and a failed gate run denies it without telling the user.

### Permitted actions

Other keys govern administering Shoal and the actions that reach beyond a
space, each `deny`, `propose` or `direct`: `admin` administering Shoal, such as `config set`, `repo add`,
`cleanup`, `setup --skip`, `contribute`, `move`, except to a joined machine but a copy of your own space there, `config setup`, `repo setup`,
and restoring removed workspaces, which reopen below your space (a
permission you set in config only to a value your space holds, and
`config reset` and `install` stay the user's), `limits` starting agents regardless of the limits, with
`--now`, `--focus` or `queue start`, `ci_approve` approving CI runs that wait
for approval, `pr_review` approving and requesting changes on PRs, bound to
the PR's head and the review's text, `triggers` pausing, resuming and running
[triggers](#triggers), `user_holds` releasing the user's holds
and removing what the user holds, and `daemon` the daemon's service
commands, `shoal update` and `shoal hub update` while a daemon of this
Shoal answers to decide it. `notifications`, `deny` or `direct`, lets a
space read the user's alerts, Shoal's own log and the inbox's
daemon-wide problems, and `decide`, `deny` or `direct`, decide the requests
of the spaces below as the user does.
With `propose` the command exits 2 with `approval_pending` and the request,
which goes to the space above whose own value is `direct`, or the user; once
approved, the same command proceeds once. A master session has every one
at `direct` but `daemon` and `triggers`, which it proposes, as it proposes
`api_writes`; other modes propose `admin`, `limits`, `ci_approve`,
`pr_review`, `triggers` and `user_holds` and are denied the rest.

## Links and watches

```sh
shoal link pr <number-or-url> assigned
shoal link ci <run-url> related
shoal unlink pr [<number-or-url>] assigned
shoal view [pr <number> | <url>] [--no-comments] [--check '<name>']
shoal watch pr [--timeout <seconds>]
```

A link names its relation last: `assigned` for the space's own work, `opened`
for an item it created another way, `related` for one the work relates to.
Each item has one relation: assigning an item replaces its opened or related
link, and an assigned item is refused another until it is unlinked.
Assigned issues accumulate per space, the first naming a new branch and
giving a new PR its title; commands that act on the assigned issue need its
number once several are. Assigned PRs accumulate per workspace, whatever their
head branch, such as a stacked PR pushed from the worktree; after `done`,
cleanup waits until all have merged and contain HEAD, or the default branch
has their changes, and a closed, unmerged PR keeps it waiting until it is
unlinked. `unlink` without a number unlinks every assigned item of the kind.

`shoal view` shows every linked item with its relation, each item's
description, comments and reviews with inline comments, and the log tails and
URLs of failed CI jobs where the forge reads job logs; `--check` selects one
check's log.

Shoal reports the activity of watched items: comments and reviews; edits to an
item's title, description, labels, assignees, milestone, draft state or base
branch; a failed CI check with its failing step when the runner records it;
the passed checks once none on the PR's head is pending; a merge conflict,
again after a push or base change that leaves it conflicting; closing and
merging. Where `[pr] watch_merge_runs` is on, the merge of a PR linked as
assigned or opened into the default branch, or a local `shoal merge` of the
space's branch that pushes it, also links the default branch's CI runs for
that commit as related, whose ends it reports, and keeps a done space until
they finish. It
leaves out what the space itself did through Shoal and the
assignees Shoal adds or removes on its own. For Claude
Code, and for a workspace's Codex in a place its host types into,
launched through Shoal, hooks deliver activity as context after tool calls and start the next
turn with it after a turn ends; a delivery replaces a running watch. Other
agents run `shoal watch pr`, which returns `updates`. Comments and edits
made before an item is linked, or before a new agent session starts, are not
reported; `shoal view` shows them. A wait for an item the space does not follow
includes its existing activity, and waits share a cursor per workspace. A newer wait
supersedes a running one, which returns `superseded: true`. Without
`--timeout` (1–3600 seconds) the wait is unbounded; `timed_out` with empty
`updates` means no update. A `lookup_failed` entry names a failed lookup,
once until the lookup succeeds or fails differently, and at most every 10
minutes per source. A `base_merged` update means the
base workspace merged; it says whether Shoal retargeted the PR and gives the
rebase command.

## CI

```sh
shoal ci ls [--pr N]
shoal view ci <run-url> [--job '<name or ID>']
shoal ci rerun [--check '<name>'] [--pr N]
shoal ci rerun <run-url> [--failed]
shoal ci cancel [--check '<name>' | <run-url>]
shoal ci run <workflow> [--input NAME=VALUE] [--branch B]
```

`view ci` lists a run's jobs with the log tails of its failed ones; `--job`
prints one job's whole log, by its name or listed ID, whatever its result. `ci rerun --check` reruns a
failed check on the linked PR's current head, and without `--check` every
failed one; `ci cancel` cancels running and waiting ones. With a run URL both act on one run. `ci run` starts
a workflow, by its file name or its link, on the space's branch, or for a chat
on the default branch, or on `--branch`. A space's agent acts on the runs of
the default branch and of the branches of the workspaces below it, and of
their PRs on any commit. It is a new run, not a rerun: on Forgejo a failed
check on the PR stays failed. When `ci run` links the run as opened, a watch
reports when it finishes. `--repo` or a run's URL names another repository,
whose runs anyone reads and only the user links and acts on. A PR from a fork or first-time contributor can show runs that
wait for approval instead of checks; `shoal view` lists them in `waiting_runs`.

## Local reviews

```sh
shoal review run                    # Or a workspace's number or ID, or a PR URL
shoal review ls                   # Reviews, newest first, with unresolved findings
shoal view review [<id>]
shoal watch review [<id>]
shoal review resolve 17.1 fixed
shoal review resolve 17.2 false-positive --reason "The caller checks the length"
shoal review resolve 17.3 wont-fix --reason "..."
shoal review resolve 17.4 deferred --issue 1240
shoal review resolve 17.5 duplicate --of 17.2
shoal review reply 17.6 "Only on empty input?"
shoal review cancel [<id>]
```

`review run` queues a one-shot, read-only review of the committed HEAD;
uncommitted edits stay out of it. With a PR URL, or from a chat for its linked
PR, it reviews the PR in the workspace that owns its branch. The findings arrive at the
workspace's agent as a message, each numbered `<review>.<n>`, and at its
coordinator as one line. `resolve` records an outcome with its data, and
`reply` discusses a finding without resolving it. Later reviews of the
repository receive each `false-positive` finding's title, path and reason and
skip that claim while the reason holds; the next review checks each `fixed`
finding. When the repository requires a review, `pr open` of a new PR,
`ready` and `merge`, of a PR or a branch, are refused until a finished review covers the commit they
act on with every finding resolved. Without one, the refusal starts a review
of the workspace's HEAD where that is the commit, and otherwise names
`shoal review run <pr-url>`.

## Ready marks

A ready mark says that a space's agent needs the user before it can go on:
its work is ready for review, or it has a decision or answer to ask them for.
`shoal ready` marks the linked items, or the space when nothing is linked,
for the user's integrations, and takes a linked draft PR out of draft unless
`[pr] undraft_on_ready = false`. It skips a linked PR that is not open or
whose head on the forge is not HEAD, naming each with its reason on stderr.
New commits leave a mark as it was; `shoal ready --undo` withdraws the marks,
and `--draft` also returns the PR to draft.

## Alerts

```sh
shoal send "<text>" [--attach FILE]   # Up: to your coordinator, else the user
shoal send --space <child> "<text>"   # Down: to the agent of a space below
shoal send --in-chat [--space S] [--attach FILE]  # Your turn's final message
shoal alert list [--unread]           # Your space's alerts
shoal alert withdraw <number>         # Take back one you sent, until it is read
```

An alert is a one-way notice. Without `--space`, `shoal send` goes up to your
coordinator, or else the user, as one line; it tells them something without
asking, and affects neither completion nor cleanup. `--space`, `--all` and
`--repo` send down to the agents below. The user, Shoal and your coordinator
send your space alerts, such as a request to free memory or disk space. Hooks
deliver them with PR activity; a new one also ends `shoal watch pr` with an
`alerts` array, and `shoal alert list` shows them. Each is shown once, and
lists each file attached to it as an `Attached file:` path. `shoal send`
prints each alert's number, by which its sender withdraws it until it is
read; `--space` names the space it went to, as for one on another machine.
`--in-chat` sends your turn's final message as the alert once the turn
ends, for an agent with Shoal's hooks: it names whom it goes to now, and
`shoal log` the alert's number once sent. A prompt the user submits during
the turn drops it and marks read the alerts your chat sent them; Shoal
tells you when your final message goes unsent, as then or when sending
fails.

## Threads

```sh
shoal thread open "<name>" [--space <space>] [--kind question|request] [--code PATH[:FIRST-LAST]]
shoal send <thread> "<text>" [--choice A --choice B --recommend A --reason R] \
  [--attach FILE] [--code PATH[:FIRST-LAST]] [--body-file F] [--wait]
shoal send <thread> --choose <N>     # Pick a choice the other side offered
shoal send <thread> --in-chat        # Your turn's final message is the post
shoal thread forward <thread> [--space <space>] [--note "<text>"]
shoal thread resolve <thread> answered [--choose N]   # A request: done; any: duplicate --of, withdrawn --reason
shoal thread resolve <thread> --undo
shoal thread list [--space <space>] [--state open|resolved|all]
shoal view thread <thread>
shoal link thread <S.N> related
shoal unlink thread <S.N>
```

A thread is a conversation between its opener and its holder, whoever is
asked now, with posts from both sides. `thread open` opens an empty thread
with your coordinator, or else the user, or with `--space` a space below
you; from the user's terminal it opens one with a space. Nobody hears of an
empty thread; each post reaches the other side as an alert, waking an idle
agent, and the user in one line. Inside a space, a bare number names a
thread of that space; others take their full `S.N`. A post states its text,
and may offer numbered choices with the one it recommends and why, or pick
one the other side offered with `--choose`; text is never read as a choice
number. `--wait` waits without a time limit for the other side's next post
and prints it. `--in-chat` makes your turn's final message the post, for an
agent with Shoal's hooks; a prompt the user submits in your chat answers it.
A turn has one such mark, a post's or an alert's: another `--in-chat`
replaces it and names the one it replaced.

`--code`, repeatable, points a post at a file, or its lines, in the worktree
of a space your scope reaches, or of the workspace your chat runs in;
`--attach` sends a copy instead. The other
side receives each as `Code: path:first-last` with the lines' snippet, and
the space whose worktree it is in when that is not its own. Shoal follows
the code as it changes: `view thread` shows each reference where its lines
are now, `changed` once lines inside it were edited and `outdated` once they
are gone, with the snippet as it was. `thread open --code` posts the
references at once, in an opening post named as the thread.

A `question` asks for an answer; a `request` asks the holder to do
something, such as implement the code it points at.

The holder forwards a thread down, to a space it reaches, or up, to its
coordinator or the user; the new holder reads what came before and receives
the posts from then on. The opener resolves its thread: `answered` a
question, `done` a request once it accepts the work,
`duplicate --of <thread>`, or `withdrawn`, which takes back the holder's
unread alerts of it. A space reads the threads it or a space below it took
part in, and `link thread` delivers a thread's new posts to it. A thread
that waits for its holder keeps the space that opened it from cleanup, and
the user's waiting threads fill the inbox. `shoal done`, and removal, close
the threads the space opened or holds, telling the other side; `done --undo`
and a restore reopen them.

## Files

```sh
shoal files [space]
shoal files add notes.md ['chart.html#Latency by region'] [--space S]
shoal files rm [FILE...] [--space S]
shoal done --attach report.md
```

`files add` copies files of at most 100 MiB into the space's attached files
without telling its agents; `PATH#TITLE` gives a title. `files` lists them, and
`rm` deletes copies. A removed space's files outlive it for the retention
period, and `shoal restore` gives them back; the coordinator, or else the user,
reads them with `shoal files <space>`.

## Completion

```sh
shoal done [space] [--summary TEXT] [--attach FILE]... [--keep-demos]
shoal done --integrated <workspace>
shoal done --undo [space]
```

`shoal done` records that the space's work is finished at its HEAD, notifies
its coordinator or else the user, closes its open threads and stops its
demos unless `--keep-demos`. `--summary` ends the notice with one line, of at
most 512 bytes, saying what the work found, and becomes the status line;
`--attach` hands back anything longer. Only `done` records completion; issue closure and
merged PRs do not. With cleanup, the default unless `[cleanup.done] enabled =
false` (`--cleanup` overrides it), Shoal removes the space and the spaces below
it after the grace period (`[cleanup] grace_minutes`), stopping their agents:
a workspace once every assigned PR has merged and contains HEAD, or without a
PR once its files are clean and every commit is pushed, on the default branch,
or on the branch it builds on; a chat whatever PRs it watches. Holds,
threads it opened that wait, and pending proposals or pushes keep a space. Each removed space but
a disposable one keeps a checkpoint and its branch, and `shoal restore`
reopens it. `completion.error` and `pr_cleanup.error` report cleanup or lookup
failures.

`--integrated <workspace>` records that another branch has that workspace's
work under new commits: until its HEAD moves, its unpushed commits no longer
keep it. `--undo` withdraws a recorded
completion and reopens the threads it closed; associations and watches stay,
and cleanup proceeds as if `done` had not run.

Where `[cleanup.merged]` is on, a merged workspace nobody has used for hours
goes with merged cleanup whether or not `done` ran.

```sh
shoal acquire hold --name <name> --reason <why> [--space <space>]
shoal leases hold
shoal release hold --name <name>
```

A hold keeps a space, a workspace or a chat, from cleanup. An agent asking to
hold its own space makes an approval request to its coordinator, or the user;
holding a space below it needs none. Scoped callers release the holds
agents placed, and the user's as their `user_holds` permission allows.

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

Removing a space removes the spaces below it first, held ones included, and
asks what to do with unmerged work. `rm` returns once its checks pass and
removes the space in the background; a later failure arrives as a message,
and `--wait` waits for the removal. A checkpoint keeps HEAD and unsaved files;
Shoal also takes one when an agent exits or is stopped. `restore <id>`
reopens a removed workspace or chat and resumes its agents, below the space it
was below, and `--no-resume` leaves them for `shoal resume`; a space that is
not removed is refused. An agent restores one whose parent its scope reaches.
`restore <space>` of a space whose `intermittent` machine is offline takes it
over here under its ID from the package the hub keeps of it, as the space's
`admin` permission allows, warning of the changes the package lacks; that
machine removes its copy once it returns.
`restore --from` opens the spaces `shoal move` wrote to a file. `cleanup`, `move`, except to a
joined machine but a `--keep` copy of your own space there, and `restore --from` are as the space's `admin` permission allows; `restore --checkpoint` lists a workspace's checkpoints,
and with a number returns to one after keeping the current state in a new
checkpoint, refusing while another command runs in the workspace. `cleanup spaces` removes every
space automatic cleanup would remove without waiting for its idle delay;
`--dry-run` lists those, kept branches and shared caches, and why each other
space stays. `move --machine` moves a space to another machine joined to the same hub,
under its ID and number, leaving the spaces below it here below it unless `--recursive`;
an agent's move of its own space there starts once its turn ends and it continues there.
`move --ssh` moves it with the spaces below it to a machine `ssh` reaches. Both resume its
agents there unless `--no-resume`; with `--keep`, they copy the spaces there as new ones
and leave them running here. The daemon runs a move, so it finishes even
after the command that started it goes.

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
about a minute. `repo sync` syncs it at once: it fetches the remote and
fast-forwards the local default branch, advancing the registered checkout when
it has that branch checked out, where `git fetch origin main:main` is refused.
It never pushes or changes a workspace's branch. Branches of other workspaces
are shared refs, and pushed ones are current as `origin/<branch>` after a sync.
`shoal refresh [workspace]` brings a workspace that holds nothing of its own up
to date with what it was made from: a branch fast-forwards to its upstream or
the branch it builds on, a workspace at a commit moves to its PR's head or
branch's tip, and a copy takes its live source's files again; work of its own
refuses it, named with what deals with it.
`conflicts` checks committed HEAD against the base workspace's branch,
otherwise the local default branch, or the branch named, without touching the
worktree; exit 1 lists the conflicting paths. It also lists a file the
repository marks as holding a number each change picks, such as a migration,
under "Changed on both sides" when both branches changed it. Without a watched
PR, the watch reports a `merge_conflict` when the check newly fails, until the
space is done.
`branch base set` records that a workspace builds on another's branch.
`branch ls` lists the forge's branches of the space's repository, or the one
`--repo` names, with each one's head commit and date, whether it is the
default or protected, whether the default branch has its changes through any
merge (unknown outside a registered repository), the open PRs whose head it
is, and the workspace whose branch it is, `active`, `done` or `merged`.
`branch delete` deletes branches on the forge, never local ones, each only
while it points to the commit it was read at, as the space's `push`
permission allows, `propose` asking once per branch. It deletes none while one is the default
or protected, the head or base of an open PR, or lacks its changes on the
default branch without `--force`, or is the branch of an active workspace
outside your scope.

`merge` merges a space's work: its one open PR linked as assigned or opened,
on the forge, else a workspace's committed branch into the default branch,
locally; several open PRs, or a chat without one, are refused, and closing the
PR lets a workspace merge locally. A number, a PR URL or `pr` merges that PR,
or with `pr` alone the assigned one. A PR merges by `-X`, else by
`[merge] method`, else by the repository's default, and `--message` sets a
squash merge's commit message; `--auto` merges it once its checks pass and
`--cancel-auto` withdraws that. A local merge takes `-X` and `--message` too,
squashing or rebasing the branch onto the default branch, and `--push` or
`--no-push`; a flag of the other path is refused. `unmerge` undoes a
workspace's latest local merge while the default branch is unpushed. A
workspace process's `merge` or `unmerge` first exits 2 with `approval_pending`
for the PR's or branch's current head, unless its `merge` permission is
`direct`; after approval the same command at the same head proceeds, and a new
commit needs a new request. `-s` acts on a space below. A merge that pushes
asks for confirmation on a terminal, and `--yes` gives it.

## Approvals

```sh
shoal approvals
shoal approvals --approve <S.N>
shoal approvals --deny <S.N>
shoal approvals withdraw <S.N>
```

A command that needs approval returns `approval_pending` with the request
number, and `approval_denied` once denied; rerunning it after approval
proceeds. `--reason` states the purpose of protected access. A request goes to
the nearest space above with an agent, then up to the user. Unscoped callers
decide any request. Approving a resource reserves no capacity.

## Ports

```sh
shoal catalog port
shoal leases port
shoal acquire port web [--port N] [--reason "purpose"]
shoal release port web
```

`catalog port` lists the configured names and defaults. Repeating a
reservation returns the same port. Exit 2 with `reserved: false` is a conflict
suggestion, not a reservation; repeating the request with
`--port <suggested_port>` reserves it. The returned `port` is the reserved one.
New reservations do not change the environment of running commands.
Reservations and leases outlive the command that made them. The simulator
leases, containers and resource permits an agent acquires end with the agent,
when it exits or is stopped; the user's stay until released.

## Resource pools

```sh
shoal catalog resource
shoal leases resource
shoal acquire resource devices [--member M] [--name N] [--mode read|write] [--queue]
shoal release resource devices [--name N]
```

The result names the leased `resource`. Each semaphore lease takes one permit;
the same `--name` returns the same lease, and release without `--name` frees
every lease in the pool. Busy requests exit 2. `--queue` waits, without a time
limit, until the lease is granted or its approval denied, taking capacity in
arrival order; stopping it gives up the place, and while others are queued,
requests without it are busy. An `rwlock` resource has shared readers and
exclusive writers (`--mode`, write by default); `read_available` and
`write_available` give its capacity, and changing mode needs a release first.
Use is cooperative.

## Simulators

```sh
shoal catalog sim
shoal leases sim
shoal acquire sim [--queue]
shoal acquire sim --clean --reason "Verify first-launch permission prompts"
shoal release sim
```

`catalog sim` lists the configured profiles and installed runtimes.
Acquisition returns an exclusive lease on a device with its `udid`; simctl's
`booted` selector can name another workspace's device. Shoal creates, boots,
shuts down, erases and deletes its devices, choosing one that minimizes
reinstalls, and normal reuse keeps apps, data and settings. Exit 2 means
capacity is busy. `--clean` erases the device, refuses while a lease is held,
and records the reason and outcome in an audit trail. Only installed runtimes
are supported.

## Containers

```sh
shoal catalog containers
shoal acquire containers [--reason "purpose"]
shoal leases containers
shoal release containers
```

Where the repository names a Compose file in `[containers]`, `acquire
containers` reserves its named ports, starts the workspace's own Compose project
and returns its services with the ports they published; repeating it returns
the running project. Exit 2 means a port it reserves waits for approval or its
preferred port is taken. `release containers` stops them, keeping their volumes
and ports, and removing the workspace removes them with their volumes. With
`start = "setup"` they start once setup readies the workspace, before its
post-setup hook and agent, and an agent starting there starts them when they
are stopped. Chats have none.

## Secrets

`shoal catalog secret` lists the configured secrets and which need approval. A
tool whose secret needs approval fails with the request to make:
`shoal acquire secret NAME --reason "purpose"`. The value never reaches the
agent's environment; `shoal release secret NAME` gives up the grant.

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

Shoal keeps a demo's command running, restarts it when it exits, and tells the
user how to open it; `shoal demo` and the inbox open it for them. The `server`
preset reserves a port and passes it as `PORT`, and `web` does the same for a
page opened in a browser; the `simulator` preset leases the workspace's
simulator and passes its UDID as `SHOAL_SIM_UDID`. `shoal demo NAME
--workspace W` on a `server`, `web` or `simulator` demo another machine of the
hub runs forwards it to a port of this machine, printing where, until stopped;
a simulator demo's is its viewer's, a page that shows the device and takes
touches. A terminal demo there opens in the calling terminal as `shoal attach`
does, so it needs one. Chats have no demos.

## Triggers

```sh
shoal trigger ls [--repo R]   # Source, paused or active, next firing, last run and its space
shoal trigger pause <name>    # It fires, and launches a queued run, no more until resumed
shoal trigger resume <name>   # It counts its times from now; those it missed never run
shoal trigger run <name>      # Fire it now and print the run's outcome
shoal trigger run <name> --payload issue.json --header X-Forgejo-Event=issues
```

A trigger, `[triggers.NAME]` in a repository's config or the global config,
starts an agent when its source fires, such as a timer at set times or an
HTTP request to its webhook route, as the
user's `shoal start` would: through the agent queue, in a top-level space with
its mode's permissions. The commands act on the triggers of the repository the
caller acts in, or `--repo` names, and the global config's, and change a
trigger's runtime state, never its config. `ls` is open to every space;
`pause`, `resume` and `run` follow its `triggers`
[permission](#permitted-actions). A run that a trigger's `overlap` skips or
queues, whose key repeats a recent run's, or whose launch fails says so; one
its filter drops is no run, and names the condition that did not hold.
`--payload` and `--header` give a webhook trigger the request it fires with,
without its signature.

## Processes

After a tool call Shoal names processes the space's commands left running
after their parent exited, such as a failed test run's servers.
`shoal stop --leftovers` stops them, and `shoal stop --processes` stops every
process of the workspace Shoal does not track, never the agent or its shells.
`shoal setup` reruns the configured setup command and post-setup hook.

## Daemon

Installing, starting, stopping and restarting the daemon, `shoal update` and
`shoal hub update`, which installs the latest release on the hub, follow the
space's `daemon` [permission](#permitted-actions).
Stopping the daemon stops every agent. While a stop or restart waits for the
work the daemon runs itself, commands that would start more, such as `shoal rm`,
wait and run on the next daemon, or fail for a stop; while it restarts, every agent's other `shoal`
commands fail. `shoal config reload` applies the config on disk without
stopping anything; an invalid file leaves the previous settings in place.

## Skill

`shoal skill` prints this skill. `shoal skill install` installs it into every
AI tool's existing skill directory, and a tool name selects one and creates
its directory; it is refused in a scoped execution. Later unscoped commands
keep installed copies current.
