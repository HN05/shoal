# Approvals

This file says what `shoal approvals` does and how an agent's issues, PRs,
pushes, merges and resources wait for approval.
[Scope, approvals and permissions](../design/permissions.md) gives the reasons.

## Approval requests

An approval request asks for a yes or no before a scoped caller acts. A space's
[permissions](modes.md#mode-permissions) decide which of its actions need one,
and the resource settings decide which leases do. Requests cover:

- issues and PRs an agent [proposes](#approval-for-agent-issues-and-prs),
- an agent's [pushes](#approval-for-agent-pushes),
- an agent's [merges and unmerges](branches.md#approval-of-an-agents-merge),
- [actions](modes.md#permitted-actions) whose permission is `"propose"`,
- [protected resources](#resource-approvals),
- a [hold](removal.md#holds) an agent asks for on its own space.

Unscoped callers act without a request.

A command that needs approval exits 2 at once, with JSON
`code: "approval_pending"` and the request. Run it again once the request is
decided. After approval, it proceeds. After denial, it returns
`"approval_denied"`. A proposal is the exception: `issue open` and `pr open`
exit 0 with `proposed`, and the agent goes on with its work.

Each request has a [number](threads.md#numbers) `S.N`, as threads have.

### Who decides

A request from a space with an agent of its own goes first to the nearest
space above it with an agent, a [workspace](create.md) or a
[chat](chats.md). That space's agent is told. It decides the request with
`shoal approvals --approve <S.N>` or `--deny <S.N>`, or passes it up with
`shoal approvals --escalate <S.N>`. A request passed up goes to the next such
space above, and from the top to you. Without a space to decide it, the
request comes to you.

A deciding space approves a request that a permission governs only where its
own effective permission is `"direct"` for every key that governs it:

| Request                          | Keys                       |
| -------------------------------- | -------------------------- |
| An issue                         | `issue_open`               |
| An issue in another repository   | `issue_other_repositories` |
| A PR                             | `pr_open`                  |
| A PR that pushes first           | `pr_open` and `push`       |
| A push                           | `push`                     |
| A merge or unmerge               | `merge` and `push`         |
| A [permitted action](modes.md#permitted-actions) | the key that governs it |

It approves any other request only where it holds an approved grant with the
same target and settings. A request to hold a workspace needs no grant.
Otherwise `--approve` is refused, citing the permission or the missing grant,
and the space denies the request or passes it up.

A space whose `decide` [permission](modes.md#permitted-actions) is `"direct"`
decides any request of a space below it as you would, routed to it or not,
with or without a grant. It still approves a request that a permission
governs only where it holds that permission at `"direct"`.

The request records the deciding space as its `decider`. You may still decide
it yourself. When a chat's agent stops, or a deciding space is removed, the
requests it had not decided come to you.

### List and decide

`shoal approvals` lists the requests and the retained
[workspace grants](#resource-approvals). A scoped caller sees the requests of
its own space and the spaces below it. `--space <space>` lists the requests a
space made or proposed and those it decides.

On a terminal, `shoal approvals` opens a picker:

- Selecting a pending request shows its recorded settings and prompts for a
  decision.
- Ctrl-A approves and Ctrl-D denies the highlighted request without leaving
  the list. The outcome or error shows above the key hints, and the list
  updates.

`shoal approvals <S.N>` shows one request the same way. The prompt takes `y`
to approve or `n` to deny, in either case. Any other input cancels. JSON and
output that is not a terminal print the records without prompting.

`--approve [S.N]` and `--deny [S.N]` decide a request. Without a number, they
offer the pending requests to pick from, each with its space, reason and
settings. A decision applies to the request you were shown: when another
request took its number meanwhile, the decision is refused.

### Alerts about requests

A pending request that waits for you gives you an alert, collapsed with the
others until read. A decision queues an [alert](alerts.md) for the requesting
space that names the request, its target and the decision.

### Outdated requests

A merge, unmerge, PR or push request binds a commit. It becomes `outdated` when
it no longer applies: a request for a newer commit replaced it, or it was found
stale when approved. It stays listed until its workspace is removed, so no
other request takes its number. Deciding it is refused.

### Withdraw a request

`shoal approvals withdraw <S.N>` takes back a pending request that the
requesting space no longer needs, as releasing a lease does for a lease
request. The request stays listed as `withdrawn`, as an outdated one does. A
withdrawn issue proposal is deleted instead, as a decided one is.

While the request's `approval_requested` alert is unread, withdrawing deletes
it. Otherwise the space deciding the request is told, or you get
`approval_withdrawn`. A command that retries the request, such as a queued
`acquire`, requests it again.

### How long requests last

Requests survive a daemon restart and a failed removal. Removing a workspace
deletes its requests, except proposed issues, which belong to the repository.
Of all requests, only pending PR proposals and push requests keep a workspace
from idle cleanup.

## Approval for agent issues and PRs

The `issue_open` and `pr_open` [permissions](modes.md#mode-permissions) decide
whether an agent opens an issue or PR at once:

- `"direct"`, the default, opens it.
- `"propose"` records a proposal, an approval request for the item, instead.

They apply to `issue open` and `pr open` in a workspace process, and to
`issue open` from a chat's agent. `issue_other_repositories` governs
[issues in another repository](issues.md#issue-and-pr-actions) the same way.
Unscoped callers always open directly.

### Propose an issue

A proposed `issue open` records every option it was given, with the space, a
workspace's branch and commit, and the agent. Running it again unchanged
returns the same request.

The proposal belongs to the repository, so it stays when the space is removed.
It keeps the space's number: no new space takes that number until the space's
last such proposal is decided. Lists show the space's name marked `(removed)`,
and `--json` gives `space_removed`.

### Propose a PR

A proposed `pr open` pushes the branch as usual, unless the
[`push` permission](#approval-for-agent-pushes) holds the push back. It links
the branch's open PR if there is one. Otherwise it records the PR it would
open, with its defaults filled in, at the branch's current commit.

Running it again at the same commit returns that request. At a newer commit it
replaces the request, and the old one becomes [outdated](#outdated-requests).
A pending PR proposal keeps the workspace from automatic cleanup.

### Attached files

`--attach` copies its files into the `.proposals` directory of the repository
the proposal is for. Shoal deletes them once no pending proposal uses them: at
the next decision or proposal, in the cleanup sweep, or when the repository's
registration is removed.

### Decide a proposal

```sh
shoal approvals                     # proposals show their labels and description
shoal approvals --approve 3.4 --title "Fix login timeout" --body-file edited.md
shoal approvals --deny 3.5 --reason "Not a bug"
shoal approvals --deny 3.6 --duplicate-of 120     # an existing issue's number or URL
shoal approvals --deny 3.7 --duplicate-of-approval 3.4
```

`--approve` opens the item as the space's agent account. `--title`, `--body`
or `--body-file` replace what was proposed.

- An issue is recorded as opened by its space, or as assigned to it when it
  was proposed with `--link`.
- A chat's issue opens through the repository's checkout, as does an issue
  whose space is removed.
- A PR is linked as one that `pr open` opened.

Approving returns the [convention](issues.md#repository-conventions) warnings
that opening prints. Shoal checked the required labels when the item was
proposed.

`--deny` drops the proposal. `--reason` gives the agent a reason.
`--duplicate-of` and `--duplicate-of-approval` deny an issue proposal as a
duplicate:

- `--duplicate-of` adds the proposal's title and description as a comment on
  that issue. For an issue in another repository, the deciding space needs
  `issue_other_repositories` at `"direct"`.
- `--duplicate-of-approval` keeps them with another pending issue proposal.
  That proposal adds them as comments when it is approved, and drops them when
  it is denied.

A decided proposal leaves the list. The decision reaches the space's agent as
an [alert](alerts.md) that names the opened item.

### Outdated PR proposals

Approving a PR proposal is refused when its branch was renamed, or has another
commit locally or where `pr open` pushed it. The request becomes outdated, and
the agent is told to run `pr open` again.

### Approval that fails partway

Labeling a PR, requesting reviews or attaching its files can fail after the PR
opened. The request then keeps the PR, and approving it again finishes that
work.

Adding an issue's labels, its files or a duplicate's comment can fail after the
issue opened. The request then keeps the issue and the work left. Approving it
again, without edits, finishes that work instead of opening another issue.

Shoal never posts a comment twice. When attaching its files or reading the
issue fails after the comment reached the forge, the decision finishes and
reports the failure in `warnings`.

## Approval for agent pushes

The `push` [permission](modes.md#mode-permissions) governs an agent's pushes to
the remotes of the workspace's repository:

- `"direct"`, the default, pushes at once.
- `"propose"` keeps the push off the forge until it is approved.
- `"deny"` refuses it.

Unscoped callers always push directly. A proposed PR still pushes its branch
unless `push` holds it back.

### Refused pushes

With `"deny"`, `shoal pr open` from a workspace process fails while the remote
lacks the branch's commit. Once you have pushed that commit, `pr open` opens or
links the PR. The agent's `git push` reports each ref as rejected, with the
reason. Shoal records no approval request.

### Held pushes from `pr open`

With `"propose"`, `shoal pr open` from a workspace process pushes nothing while
the remote lacks the branch's commit:

- Without an open PR, it records the PR as a proposal that pushes the branch
  first, whatever `pr_open` says. One approval pushes the branch and opens the
  PR.
- With the branch's PR open, it links that PR and records a push request for
  the branch at its current commit.

### Held pushes from `git push`

With `"propose"`, Shoal also holds an agent's own `git push`. It adds Git
settings to the agent's environment that rewrite the push URLs of the
repository's remotes to Shoal's remote helper, `git-remote-shoal`, which it
puts first on the agent's `PATH`. Fetches keep their URLs.

Git runs its usual checks and pre-push hook, then hands the push to the helper.
The helper asks the daemon about each ref. A push that may go ahead goes to the
real URL, with Git as the agent. Any other push becomes an approval request,
and Git reports it as rejected:

```text
 ! [remote rejected] HEAD -> topic (waits for approval 12)
```

The request binds:

- the remote, and the push URL when the remote has several,
- the ref and the commit,
- for a forced push or a deletion, the remote commit it replaces, or that the
  ref must not exist yet.

`--force-with-lease` works as usual. `--dry-run` and push options are refused.
Pushing the same again names the same request. Pushing another commit to the
ref replaces the request, and the old one becomes
[outdated](#outdated-requests). A pending push keeps the workspace from
automatic cleanup.

Shoal cannot hold an explicit push URL that a fetch URL starts with, such as
one that is also fetched from: Git would rewrite that fetch URL too. The agent
does not start until that push URL changes or `push` is `"direct"`.

An agent's rewrite follows the `push` permission it
[launched with](modes.md#mode-permissions). The rewrite stops pushes by accident, not on purpose: an agent that clears its Git
settings, or pushes to a URL spelled differently, reaches the remote.

### Approve a push

```sh
shoal approvals                     # pushes show the ref, commit and remote
shoal approvals --approve 12        # pushes it, then opens a PR that waited for it
shoal approvals --deny 12 --reason "Squash the fixups first"
```

Approving runs the push in an operation the daemon starts, as a tracked command
in the workspace. It runs in your environment when you approve from a
terminal, and in the daemon's otherwise, as when a space above approves it.
It pushes with the agent's [Git profile](agents.md#agent-accounts) and its
credentials, as `pr open` does.

The push sends the commit you approved, not whatever the branch has by then.
When the workspace's local branch moved since the request, approving its push
is refused, the request becomes outdated, and the agent is told to push again.

`--approve` follows the push and returns once the push has decided the request:

- A push of the workspace branch to a configured remote sets that upstream,
  unless the branch has one.
- A PR proposal that waited for the push then opens its PR.
- A failed push leaves the request pending and notifies you. Approve it again
  to retry.

While the push runs, Shoal refuses to deny or withdraw the request, and refuses
new push requests and PR proposals from the workspace. The agent hears the
outcome as an [alert](alerts.md).

### Merges and reverts

Unless `push` is `"direct"`, a scoped local `shoal merge` needs
[approval](branches.md#approval-of-an-agents-merge). A scoped `shoal pr revert`
through the forge is refused, since the forge pushes its branch at once.

## The gate

A permission of `"gate"` lies between `"direct"` and `"propose"`. An agent, the
gate, decides the request before anyone else sees it.
[Permissions](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#permissions)
names the keys that take `"gate"`, and [reviews](../design/reviews.md) gives
the reasons for the gate.

### Requests the gate decides

A request goes to the gate when the narrowest of the permissions that
[govern it](#who-decides) is `"gate"`. A merge is governed by both `merge` and
`push`, for example. When one of them is `"propose"`, the request takes the
`"propose"` route instead.

A space that holds `"gate"` or `"direct"` may give a child `"gate"`.

### Verdicts

The command records the request as it would for approval. The gate decides it
at once, in the background, with one of these verdicts:

- Allow: the request is approved as your approval would approve it, pushing,
  opening or merging.
- Deny, with a reason.
- Pass on: the request takes the `"propose"` route, to
  [the space that decides](#who-decides) or to you. It carries the gate's
  reason and the case it quoted.

The gate never edits a proposal.

### What the gate checks

The gate is no code reviewer. It checks that the action follows the
repository's conventions and is not a mistake, such as a push to the wrong ref,
a merge with failing checks, a duplicate issue or work unrelated to the
space's.

The gate decides every request itself, sure or not. It passes one on only for a
case that `escalate` in the
[`[gate]` table](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#gate)
lists, and quotes that case word for word. Shoal refuses an escalation that
quotes none and asks the gate once more, telling it why.

### What the gate reads

The gate is given:

- the request in full,
- the space's name and mode,
- its linked issues and PRs, with their labels, CI and review state,
- the commits the request brings, as `git log --stat`,
- what the request leaves out of the repository's
  [conventions](issues.md#repository-conventions).

To look further, such as at another PR, CI logs or an earlier proposal, the
gate runs the `shoal` commands that only read, such as `shoal view`,
`shoal status`, `shoal ci ls` and `shoal issue ls`. It runs them under a
read-only scope of the requesting space, which ends with the run. The daemon
refuses that scope every request that changes anything.

### How the gate runs

The gate is a one-shot run of `[gate] agent`, else `agent.default`, through the
tool's `gate` [template](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools).
It runs read-only in the repository's registered checkout, and reads the
`[gate]` rules there, never the requesting workspace's. A run lasts at most 10
minutes. It holds no heavy slot and waits only for memory and disk room.

### Gate sessions

The gate keeps one session per space and subject. A space's subjects are:

- its own branch, with its PR, merges and unmerges,
- its issue proposals for each repository,
- its pushes to each other ref.

A later request of a subject resumes its session through the tool's
`gate_resume` template. The gate is then given only what is new, such as the
commits since its last verdict. A new session starts when the agent or its
rules change, or when the tool can no longer resume the session. The sessions
go with the space.

### Denials and failures

A denial reaches the agent as an [alert](alerts.md) with the reason. The agent
changes what the reason names and runs the command again, which makes a new
request.

After three denials of the same action, the command is refused without asking
the gate until the action changes. The same action is:

- a push of the same ref at the same commit,
- a PR proposal with the same title, description and labels at the same
  commit,
- an issue with the same repository and title.

When the gate times out, crashes or returns no verdict, `fallback_agent`
decides once more. When that fails too, the request is denied with the
failure. Such a denial does not count toward the three, and you are not told.
The agent may tell you with `shoal send` or ask in a [thread](threads.md).

When the gate cannot be set up at all, as without an agent it can run, the
request is denied and you are told.

### What leaves the gate

These requests take the `"propose"` route, to the space that decides or to
you:

- a request the gate passes on,
- an allowed request that cannot be carried out, as when its push cannot
  start, with the error, which its agent is told too,
- an issue proposal whose space is gone, which the gate does not judge.

Every verdict goes to the space's [log](log.md) as `gate_decided`, with the
reason, agent and session. It also goes to the agent as a message, and to its
coordinator as one line.

## Resource approvals

[`requires_approval`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#approvals)
on a resource, port, simulator or tool-scoped secret makes a scoped acquisition
an approval request. `approval_lifetime` says how long the approval lasts. With
`"workspace"`, the approved grant stays for later acquisitions, and
`shoal approvals` lists it.

### What an approval binds

- A port approval binds the preferred port, the automatic range, the
  environment variable and the conflict policy. Accepting a conflict
  suggestion with changed settings needs a new request.
- A simulator approval binds the installed device and runtime, and whether a
  clean device was requested. A clean request still needs a reason and the
  usual audit before the device changes.
- Selecting a protected profile's device and runtime directly does not relax
  the profile.
- Where policies overlap, `"lease"` lifetime wins over `"workspace"`.

Changed effective settings cannot reuse a grant.

### Acquire with approval

An agent runs the usual acquire command with `--reason`. The command returns
at once, as [any request](#approval-requests) does, and the agent acquires
again after the decision. `--queue` instead waits for the decision and for
capacity, and stops on denial.

Approval reserves no capacity. Retrying with the same name keeps the request.
Changing its settings or reason needs a release first. Release cancels a
pending or denied request, even without a lease.
