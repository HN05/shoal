# Scope, approvals and permissions

This file covers what scoped callers may do, how they ask for approval, the
permissions of spaces, proposals and push requests.

Tracked executions and processes started with exported workspace environments
inherit a daemon-validated scope token that
confines them to their own workspace and the spaces below it:
status, inspect, execute, setup, and resources.
Creation other than of a child of the caller's space, removal other than of a descendant the user holds nothing below,
reconciliation, other workspaces, repository administration, and service control
need an unscoped caller; `config reload`, which applies the config on disk and
stops nothing, does not. A scoped `merge` is an
approval request bound to the branch and its HEAD unless the caller's `merge`
and `push` permissions are `"direct"`, and a scoped `unmerge` one bound to the merge it undoes;
approval lets the agent's retry merge that commit, or undo that merge, through the normal checks,
so it replaces only the user typing the command. PR registration,
manual merge acknowledgement, assignment completion and its withdrawal, and syncing the
caller's own repository are own-workspace exceptions.
Effective configuration may be read for the caller's own workspace; changing it
needs an unscoped caller.
Nested executions keep scope, and the CLI refuses scoped operations to a process
whose ancestor carries scope for the same state directory, so removing the token
from a descendant's environment does not lift them. A tracked execution's wrapper
records its own process identity in the state directory before it spawns the
command, until the command exits, and the command's identity while it lives,
because macOS hides the environments of its own platform binaries, such as
shells; an exported environment is recognised only where an ancestor's
environment is readable. `internal env` exports a space's identity, and for a
workspace its ready, verified worktree's variables and current port variables,
with a fresh token, for any space the caller's scope reaches.
Exported tokens persist across restarts until explicitly revoked or the space
is removed; they register no execution and do not prevent cleanup.

Approvals are the daemon-owned way a scoped caller asks the user for a yes or no;
`shoal approvals` opens a request picker on a terminal, and a numbered request
shows its details and prompts for a decision. Ctrl-A approves and Ctrl-D denies
the highlighted request inside the list, which fzf updates in place rather than
closing and reopening. Decisions bind to the displayed or
authorized record through mutation, including when a coordinating agent decides. Explicit `--approve` and `--deny` flags decide every kind: merging
a branch (see scope), resource access, and issues and PRs an agent proposed.

Permissions belong to spaces, not repositories, because a top-level chat the
user talks with may be trusted with more than a worker several levels down. A
space's set comes from its agent's mode: `[modes.NAME.permissions]` over
`[permissions]` over the built-in mode's defaults over each key's default. Each
key is then the narrower of that and the parent's effective value, so a child
gets no more than its parent from its mode. The modes that coordinate default
`issue_other_repositories` to `"direct"` so they can decide their children's;
`worker` keeps the key defaults, and so does a mode the user defines. `shoal start --permission KEY=VALUE` gives an
agent a permission, recorded with its mode since it belongs to that agent as
its mode does: restores and restarts keep both, and a new agent in the space
brings its own. A given value applies as given, whatever the mode or the
parent allows, because it is a decision about that agent, not a default: the
user may trust one worker to merge, or a parent give a child less than it has.
What bounds it is the giver: a value is accepted only from one who holds it,
the user, who holds every permission, or a space's agent up to its own
effective value. The daemon checks this wherever a scoped process brings a
value in; a restore or registration may also bring back what the space
recorded, but nothing more, so an agent cannot raise its own permissions by
replaying a record, and a reattaching wrapper keeps the value its execution
recorded. A value recorded before given values applied as given, when each
check capped it at the parent's, applies as given since: the records name no
giver to check it against, and none was ever wider than its mode. `shoal resume` and `shoal restart` take `--permission` too and change
the given values per key, `KEY=mode` dropping one, so a parent changes what a
child may do without ending its session or restating the rest; the mode stays,
since changing it would change the agent's rules mid-session. A dropped key
falls back to the space's value without one, which the giver must hold as it
holds a value it sets, so a drop cannot hand a child what a space between them
was given. The change goes
into the record a restore reads, so a restore here, in the background or from
the queue runs with it alike. A wrapper that restores its own agent, after an
in-place restart or a protection stop, restores it under the values that
record holds then, as a new wrapper would, rebuilding its push rewrites when
they changed; the daemon claims the record until the agent starts, so no
change lands unseen. A wrapper of a release before that would keep the push
rules it launched with, so its restore of a changed agent fails rather than
half apply it, leaving the record to a new wrapper. Effective
permissions are computed at each check from the current config, the recorded
modes and given values and the tree, never stored, so a config change reaches
running agents; only the push rewrites, which are
Git environment fixed at launch, wait for a restart. A check uses the acting
space's permissions, so a parent acting on a child acts under its own.

What only the user could do, such as administering Shoal, stopping the
daemon, releasing the user's holds, reading alerts, starting agents
beyond the limits, approving CI runs, merging PRs and deciding any request
below, is an action a permission key governs, because which spaces an
agent acts on is a question of reach and which actions it takes is one of
trust, which the user sets per mode. A master session gets every key at
`"direct"` but those whose action stops or bypasses every agent, `daemon`
and `api_writes`, which it proposes; other modes propose the keys whose
action a parent can judge and are denied the rest, so the user keeps an
action by setting its key to `"propose"` or `"deny"`. With `"propose"`
the action becomes an approval request bound to what it does, which lets
the same action proceed once, so an approval never covers a later, different
action. Deciding is a key of its own, `decide`, so a master session
decides what reached a space between it and the requester, grants
included, as the user would, but never its own requests, and approves an
action only where it holds that action's key at `"direct"`, so a key it
proposes never passes through a child it starts. The daemon checks
every request it receives, so a protocol client cannot step around the CLI;
the CLI asks the daemon before what a command does itself, such as writing
the config, so the two never disagree. Since config sets permissions, a
scoped process edits it key by key and sets a permission only to a value it
holds, as it gives one with `--permission`; replacing the file could drop a
narrowing, so that stays the user's. A setup, whose agent edits the file, starts
as `admin` allows, so a space that does not hold `admin` at `"direct"` leaves
it to the approval of whoever decides for it. A command sends several requests, so
the approval of the command grants a scope token its requests carry, which
takes that key's actions without asking again for an hour at most and dies
with the daemon. Forge writes these keys allow go out as the agent account,
so the forge shows who made them.

In a shared repository everything an agent opens is seen by other people, so
the `issue_open` and `pr_open` permissions can make a scoped `issue open` or `pr open`
propose instead of open (`"direct"` is the default; unscoped callers always open).
A proposal is an approval request whose record carries the command's options with
their defaults resolved, the workspace, branch, commit and agent, and copies of its
attachments in its repository directory's `.proposals` until no pending proposal
uses them, which a move of that directory carries over; the command
reports it and exits 0, because the agent goes on with its work and hears the
decision as a message. A proposal is routed like any request, to the nearest
space above with an agent and up to the user. Publishing to a forge other people see is the review
the setting asks for, so a space above approves one only where its own
permission for it is `direct`: it may then publish the item itself, so
reviewing a child's is within what it may do. A parent without that permission may deny it, with a reason or as a
duplicate, or pass it up; a duplicate only adds a finding to an existing issue
or proposal, which opens nothing. Approval carries data: the decider may replace the title and body, and the daemon then opens the item as the agent account through the same path
`issue open` and `pr open` use, recording and linking it. Denial may carry a reason,
and an issue proposal may be denied as a duplicate of an existing issue, which gets
its text as a comment, or of another pending proposal, which posts it once opened.
A decided proposal is deleted, since the message carries the outcome. An issue
proposal belongs to the repository rather than the space that made it, because
agents file follow-ups just before `done`; it opens through the repository's
checkout and agent account once the space is gone. A PR proposal is bound to the branch and its
commit like a merge request: the push still happens unless the `push` permission
holds it back, and a renamed branch or another
commit, locally or where the push went, outdates it at approval or when the agent
proposes again, and a pending one keeps the workspace
from unattended removal. Other agent writes, such as comments, edits and closing,
stay direct.

A proposed PR still publishes its branch, and agents push with plain Git too, so
a `push` permission of `"propose"` holds a scoped caller's pushes to the
workspace repository's remotes for the user (`"direct"` is the default; unscoped
callers always push); `"deny"` refuses them outright for repositories that
should get no agent push, through the same helper and `pr open` check, recording
nothing. A push request is a proposal, decided only by the user, and binds
exactly what the push changes: the remote and, where it has several, the push
URL, the ref, the object, and for a forced push or a deletion what it replaces,
so approval cannot push something the user did not see. A newer push to the same
ref outdates it, as does a moved branch at approval. A new PR whose push is held
becomes one PR proposal carrying the push, whatever `pr_open` says, since it
cannot open first, so one approval pushes and opens. Plain `git push` is covered
by rewriting the agent's push URLs, through its Git environment, to a Shoal
remote helper that asks the daemon about each ref. The rewrite is chosen over a
pre-push hook because `--no-verify` skips hooks and a Shoal hook would replace
the repository's own; like scope, it stops accidents, not a process that clears
its Git settings. A push URL that a fetch URL starts with cannot be rewritten
without breaking fetches, so the agent launch refuses it rather than leave those
pushes unheld. Approval runs the push without the agent, in an operation whose
end decides the request, with the agent's Git profile as `pr open`'s push has.
Nothing decides, withdraws or replaces a request while its push runs, since the
push lands regardless. Pushes Shoal authorized itself run without the rewrites.
Unless `push` is `"direct"`, a scoped `merge` needs approval whatever the
`merge` permission says, since it may push the default branch, and a forge's `pr
revert`, which pushes at once, is refused for scoped callers. A pending push
request keeps the workspace from unattended removal.

Approving a merge request whose branch moved since it was requested is refused
as stale and marks it outdated.

Configured resources may require human approval for scoped acquisition. The
daemon persists a request with a caller-supplied reason and the effective
allocation settings; only an unscoped caller decides it, or the nearest space
above the requesting workspace with an agent: a workspace approves only what it
holds an approved grant for with the same target and settings and otherwise
denies or passes the request up, as a chat does; the user decides at the top. A
resource grant is the only measure of what a parent may hand on, since resources
are not permissions. The decision is sent to the requesting workspace's agents
as a message so they need not poll. Pending requests consume no capacity.
Approval lasts until release by default, or for the workspace when configured,
limited to the same resource and access settings. Changed settings require a new
request; approval never overrides capacity, scope, or simulator cleanup rules.
Requests and grants survive restart and failed removal, and disappear with
successful removal. Approval lookups validate the selected ID or active
workspace/target/name record. Grant reuse validates all records for the
workspace and target, including released grants, before matching typed settings.
Malformed history outside those selections does not block access; listings
validate every selected record.
