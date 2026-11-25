# Approvals

This file says what `shoal approvals` does and how agents' issues, PRs, pushes
and resources wait for approval.

## Approval for agent issues and PRs

A repository whose issues and PRs other people see can require your approval
before an agent opens one. `"direct"`, the default, opens at once; `"propose"`
makes `issue open` or `pr open` in a workspace process, and `issue open` in a
chat's agent, record an [approval request](#resource-approvals) instead. Unscoped callers always open
directly. Like every key below, these are the space's
[permissions](modes.md#mode-permissions), which its mode and the space above it may narrow.

A proposed `issue open` records every option it was given, with the space, a
workspace's branch and commit, and the agent; running it again unchanged returns
the same request. The request belongs to the repository, so it stays when the
space is removed, keeping its number: no new space takes that space's number
until its last such proposal is decided, and lists show it with the space's
name marked `(removed)` (`space_removed` in `--json`).
A proposed `pr open` pushes the branch as usual, unless
the [`push` permission](#approval-for-agent-pushes) holds the push back, links the branch's open PR if
there is one, and otherwise records the PR it would open, with its defaults
filled in, at the branch's current commit. Running it again at the same commit
returns that request; at a newer commit it replaces it, and the old one stays
`outdated`. A pending PR request keeps the workspace from automatic cleanup.
Files given with `--attach` are copied into the `.proposals` directory of the
repository the proposal is for, since the agent's copies may be gone when you
decide, and deleted once no pending proposal uses them, by the next decision or
proposal or by the cleanup sweep, or with the repository's registration. Both commands exit 0 with JSON `proposed`, the request, and the agent
continues its work. A proposal is routed like other
[approval requests](#resource-approvals): the nearest space above with an agent,
a workspace or a chat, decides it before you do. A parent space whose own
[permission](modes.md#mode-permissions) for it is `direct` approves it as you would,
with `--title` and `--body`, or denies it; one without may deny it, with
`--reason`, `--duplicate-of` or `--duplicate-of-approval`, or pass it up.
Marking a proposal for another repository a duplicate of an issue there
comments on that issue, so it takes `issue_other_repositories = "direct"`.

```sh
shoal approvals                     # proposals show their labels and description
shoal approvals --approve 3.4 --title "Fix login timeout" --body-file edited.md
shoal approvals --deny 3.5 --reason "Not a bug"
shoal approvals --deny 3.6 --duplicate-of 120     # an existing issue's number or URL
shoal approvals --deny 3.7 --duplicate-of-approval 3.4
```

`approve` opens the item as the space's agent account, with `--title`,
`--body` or `--body-file` replacing what was proposed. An issue is recorded as
opened by its space and assigned when it was proposed with `--link`; a chat's opens through the repository's checkout, as
does any after its space is removed. A PR is linked
like one `pr open` opened. Approving a PR request whose branch was renamed, or has
another commit locally or where `pr open` pushed it, is refused, the request
becomes `outdated`, and the agent is told to run `pr open` again. When labeling
the PR, requesting reviews or attaching its files fails after it opened, the
request keeps the PR, and approving it again finishes that work. `deny` drops the proposal, with an optional `--reason` for the agent.
`--duplicate-of` denies an issue proposal and adds its title and description as a
comment on that issue; `--duplicate-of-approval` keeps them with another pending
issue proposal, which adds them as comments when it is approved and drops them
when it is denied. When anything fails after the issue opened, such as adding its
labels, its files or a duplicate's comment, the request keeps the issue and the
work left, so approving it again, without edits, finishes that work instead of
opening another issue. A comment that reached the forge is never posted again:
when attaching its files or reading the issue afterwards fails, the decision
finishes and reports the failure in `warnings`. Approving also returns the
[convention](issues.md#repository-conventions) warnings that opening prints; required
labels were checked when the item was proposed. A decided proposal is no longer listed, and the decision
reaches the space's agent as an [alert](alerts.md) naming
the opened item.

## Approval for agent pushes

A proposed PR still shows its branch and commits on the forge before you
decide, and an agent can push with plain `git push` at any time. A `push`
permission of `"propose"` keeps an agent's pushes to the workspace repository's remotes
off the forge until you approve them; `"deny"` refuses them; `"direct"`, the
default, pushes at once. Unscoped callers always push
directly.

With `"deny"`, `shoal pr open` from a workspace process fails while the remote
lacks the branch's commit and opens or links the PR once you have pushed it,
and the agent's `git push` reports each ref as rejected with the reason. No
approval request is recorded. The rest of this section describes `"propose"`.

`shoal pr open` from a workspace process then pushes nothing while the remote
lacks the branch's commit. Without an open PR it records the PR it would open as
a proposal that pushes the branch first, whatever `pr_open` says, so one approval
pushes the branch and opens the PR. With the branch's PR already open, it links
that PR and records a push request for the branch at its current commit.

A tracked agent's own `git push` is covered too. Shoal adds Git settings to the
agent's environment that rewrite the push URLs of the repository's remotes to
Shoal's remote helper, `git-remote-shoal`, which Shoal puts first on the agent's
`PATH`; fetches keep their URLs. An explicit push URL that a fetch URL starts
with, such as one also fetched from, cannot be held, since Git would rewrite
that fetch URL too, so the agent does not start until that push URL changes or
the setting is `"direct"`. Git runs its usual checks and pre-push hook,
then hands the push to the helper, which asks the daemon about each ref. A push
that may go ahead goes to the real URL with Git as the agent; any other is
recorded as an approval request bound to the remote, the push URL when the
remote has several, the ref, the commit and, for a forced push or a deletion,
the remote commit it replaces or that the ref must not exist yet, and Git
reports it as rejected:

```text
 ! [remote rejected] HEAD -> topic (waits for approval 12)
```

`--force-with-lease` works as usual; `--dry-run` and push options are refused.
Pushing the same again names the same request, and pushing another commit to
the ref replaces it, keeping the old one `outdated`. A pending push keeps the
workspace from automatic cleanup. The setting reaches agents started after it
changed. The helper keeps agents from pushing by accident, not on purpose: an
agent that clears its Git settings, or pushes to a URL spelled differently,
reaches the remote.

```sh
shoal approvals                     # pushes show the ref, commit and remote
shoal approvals --approve 12        # pushes it, then opens a PR that waited for it
shoal approvals --deny 12 --reason "Squash the fixups first"
```

Approving runs the push in an operation the daemon starts, as a tracked command
in the workspace with your environment when you approve from a terminal, the
daemon's otherwise, and the agent's [Git profile](agents.md#agent-accounts), whose
credentials it pushes with as `pr open` does. It pushes the commit you
approved, not whatever the branch has by then; a push of the workspace branch
whose local branch moved since is refused and marked `outdated`, and the agent is
told to push again. `approvals --approve` follows the push and returns once it
has decided the request. When it pushed the workspace branch to a configured
remote, the branch gets that upstream unless it has one. A PR proposal then opens
its PR as when approving any proposal. A failed push leaves the request pending
and notifies you; approve it again to retry. While the push runs, denying or
withdrawing the request, and new push requests and PR proposals from the
workspace, are refused until it ends. The agent hears the outcome as an
[alert](alerts.md). Unless `push` is `"direct"`, a scoped
local `shoal merge` also needs your approval, since it may push the default branch, and
`shoal pr revert` through the forge is refused, since the forge pushes its
branch at once.

## The gate

A permission of `"gate"`, between `"direct"` and `"propose"`, has an agent, the
gate, decide an agent's issue and PR proposals, pushes, merges and unmerges
before anyone else sees them. It applies to `issue_open`, `pr_open`,
`issue_other_repositories`, `merge` and `push`; no built-in mode sets it. A
request is the gate's when the narrowest of the permissions that govern it is
`"gate"`, such as both `merge` and `push` for a merge; with one of them
`"propose"`, it is routed as the sections above describe. A space holding
`"gate"` or `"direct"` may give a child `"gate"`.

The command records the request as it would for approval, and the gate decides
it at once, in the background: it allows it, which approves it as your approval
would, pushing, opening or merging; denies it with a reason; or passes it on,
which routes it as a `"propose"` request to the nearest space above with an
agent, and to you at the top, carrying the gate's reason and the case it
quoted. It never edits a proposal. It is no code reviewer: it checks that the
action follows the repository's conventions and is not a mistake, such as a
push to the wrong ref, a merge with failing checks, a duplicate issue or work
unrelated to the space's. It decides every request itself, unsure or not, and
passes one on only for a case the [`[gate]` table](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#gate) lists in
`escalate`, quoting it word for word; an escalation quoting none is refused
and the gate asked once more, told why.

The gate reads the request in full, the space's name, mode and linked issues
and PRs with their labels, CI and review state, the commits the request brings
as `git log --stat`, and what the request leaves out of the repository's
[conventions](issues.md#repository-conventions). It runs as a one-shot run of
`[gate] agent`, else `agent.default`, through its `gate` template, read-only
in the repository's registered checkout, whose `[gate]` rules it reads rather
than the requesting workspace's, for at most 10 minutes. To look further, such
as at another PR, CI logs or an earlier proposal, it runs the `shoal` commands
that only read, such as `shoal view`, `shoal status`, `shoal ci ls` and
`shoal issue ls`, under a read-only scope of the requesting space that ends
with the run: the daemon refuses that scope every request that changes
anything. It holds no heavy slot
and waits only for memory and disk room. Its runs keep one session per space
and subject: the space's own branch, with its PR, merges and unmerges; its
issue proposals for each repository; and its pushes to each other ref. A later
request of a subject resumes its session through the tool's `gate_resume`
template and is given only what is new, such as the commits since the last
verdict; a change of the agent or of its rules, or a session the tool can no
longer resume, starts a new session, and the
sessions go with the space.

A denial reaches the agent as an [alert](alerts.md) with the
reason; the agent changes what it names and runs the command again, which is a
new request. After three denials of the same action, the same ref at the same
commit, a PR proposal with the same title, description and labels at the same
commit, or an issue by its repository and title, the command is
refused without asking the gate until the action changes. When the gate times
out, crashes or returns no verdict, and then `fallback_agent` does too, the
request is denied with the failure, which does not count toward the three; you
are not notified, and the agent may tell you with `shoal send` or ask in a
[thread](threads.md). When the gate cannot be set up at all, as without an
agent it can run, the request is denied too and you are notified, since only the
config fixes that. An allowed request that cannot be carried out, as when its
push cannot start, takes the normal route instead, with the error, and its agent
is told, and so does an issue proposal whose space is gone, which the gate does
not judge. Only those and escalations reach you, as approval requests in
`shoal approvals` and the inbox. Every verdict goes to the space's
[log](log.md) as `gate_decided`, with the reason, agent and session, to
the agent as a message, and to its coordinator as one line.

## Resource approvals

[`requires_approval`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#approvals) on a resource, named port, simulator
profile, `[simulators]` or tool-scoped secret gates scoped acquisition, for as
long as its `approval_lifetime` says. A protected machine profile cannot be
relaxed by selecting its device/runtime directly; where policies overlap, lease
lifetime is stricter than workspace lifetime. Unscoped acquisition needs no separate approval. Port approvals bind the preferred
port, automatic range, environment variable, and conflict policy; an accepted conflict
suggestion with changed settings requires a new request. Simulator approvals bind
the installed device/runtime and whether a clean device was requested; clean requests
still need a reason and the usual audit before mutations.

Agents use the normal acquire command with `--reason`. `shoal approvals` shows
these requests, merge requests from a scoped `shoal merge`,
[proposed issues and PRs](#approval-for-agent-issues-and-prs), and retained workspace grants; `shoal approvals --space <space>` lists
the requests a space made or proposed and those it decides. Scoped callers can inspect requests in their workspace tree. On a terminal,
`shoal approvals` opens a picker; selecting a pending request or naming its
[number](threads.md), such as `shoal approvals 3.2`, shows its recorded settings and prompts for `y` to approve or `n` to deny,
case-insensitively; any other input cancels. In the list, Ctrl-A approves
and Ctrl-D denies the highlighted request without leaving it: the outcome or
error shows above the key hints and the list updates. JSON and nonterminal output
print records without prompting. An unscoped user
reviews the recorded settings and uses `shoal approvals --approve [S.N]` or
`shoal approvals --deny [S.N]`; without a number, they pick among pending requests, shown
with their space, reason and settings, and a request that took the chosen
number meanwhile is refused. A keep or merge request that no longer applies, because a request
for a newer commit replaced it, its completion was withdrawn or it was found stale,
stays listed as `outdated` until the workspace is removed, so its number is not reused and deciding it is refused. Approval reserves no capacity: retry acquisition after
the decision. Pending requests produce a daemon alert, collapsed until read;
a decision queues an [alert](alerts.md) for the requesting space
naming the request, its target and the decision.

`shoal approvals withdraw <S.N>` lets the requesting space
take back a pending request it no longer needs, as releasing a lease does for a
lease request. The request stays listed as `withdrawn` on the same terms as
`outdated`, except a proposed issue, which is deleted as a decided proposal is.
Withdrawal deletes the request's `approval_requested` alert while it is
unread; otherwise its deciding coordinator is told, or the user gets
`approval_withdrawn`. A command that retries the request, such as a queued
`acquire`, requests it again.

A request from a space with an agent of its own goes first to the nearest
space above it with an agent: a parent [workspace](create.md) or a
[chat](chats.md), whichever comes first. That space's
agent is told and decides it instead of the user, by number with
`shoal approvals --approve <S.N>` or `shoal approvals --deny <S.N>`, or
passes it up with `shoal approvals --escalate <S.N>`, to the next such
space above and at the top to the user. A parent space, workspace or chat,
approves a request a permission governs, an issue or PR proposal, a push, a
merge or an unmerge, where its own effective [permission](modes.md#mode-permissions)
for it is `direct`: `issue_open` for an issue, or `issue_other_repositories`
for one in another repository, `pr_open` for a PR, `push` for a push and for
a PR that pushes first, `merge` and `push` for a merge or unmerge, and the
key that governs an [action](modes.md#permitted-actions) for it. It
approves anything else only where it holds an approved grant for it, with the
same target and settings, or a request to hold the workspace, which needs
none. Otherwise `--approve` is refused, citing the permission or the missing
grant, and it denies or passes the request up. A space whose `decide`
[permission](modes.md#permitted-actions) is `"direct"` decides any request of a
space below it as you would, routed to it or not, and with or without a
grant, but approves one a permission governs only where its own value for
it is `"direct"`. A push a parent approves runs
in the daemon's environment. When a chat's agent stops,
or a space deciding a request is removed, such as below an issue
proposal that outlives its workspace, the requests it had not decided go to
the user. Without a deciding
space, the user is notified as above. The request records its `decider`, the
deciding space, while one decides it; the user may still decide it.

An acquisition waiting for a decision returns exit 2 and JSON
`code: "approval_pending"` with the request; denial returns `"approval_denied"`.
`--queue` waits for pending approvals and capacity, stopping on denial. Retrying the same
name preserves the request; changing its settings or reason requires release first.
Release cancels a pending or denied request even without a lease. Changed effective
settings cannot reuse a grant. Requests survive restart and failed removal and are
deleted with the workspace, except proposed issues, which belong to the repository;
only pending PR proposals and push requests prevent idle cleanup.
