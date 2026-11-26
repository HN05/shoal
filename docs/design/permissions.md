# Scope, approvals and permissions

This file covers what a scoped caller may do, how it asks for approval, the
permissions of spaces, and proposals and pushes.
[Approvals](../reference/approvals.md) and
[Mode permissions](../reference/modes.md#mode-permissions) say what the
commands do.

## Scope

### The scope token

A workspace's processes and every chat's agent carry a scope token, which the
daemon validates. A chat's token lasts as long as the chat. Only the user's own
terminal is unscoped. [Spaces](spaces.md) says which spaces a scope reaches.
Scope is cooperative, as
[Scope is cooperative](overview.md#scope-is-cooperative) says.

### Nested processes keep scope

A nested execution keeps its scope. The CLI refuses scoped operations to a
process whose ancestor carries scope for the same state directory. Removing the
token from a descendant's environment thus lifts nothing.

A tracked execution's wrapper records its own process identity in the state
directory before it spawns the command. It keeps that record until the command
exits, and records the command's identity while it lives. The records are
needed because macOS hides the environments of its own platform binaries, such
as shells. An exported environment is recognized only where an ancestor's
environment is readable.

### Exported environments

An app starts its own processes in a space, outside any tracked execution.
`shoal internal env` gives it the space's environment with a fresh token, for
any space the caller's scope reaches.
[Export a space's environment](../reference/modes.md#export-a-spaces-environment)
says what it holds and how long the token lasts. An exported token registers
no execution, so it keeps no space from cleanup.

### Creating and removing spaces

A scoped caller creates only children of the spaces its scope reaches. Only the
user's terminal creates top-level spaces. A scoped caller runs chats only in
worktrees its scope reaches, and never takes over a workspace outside it.
[Spaces](spaces.md) says where a new space goes.

The daemon confines a scoped caller's resources to its own worktree. It refuses
the caller every allocation, removal and recovery of a workspace, except:

- creating a child of a space its scope reaches,
- removing a space below its own,
- restoring a removed space below its recorded parent, where its scope reaches
  that parent.

A scoped caller releases the user's holds, and removes a space the user holds
or one with such a space below it, only as its `user_holds` permission allows.
Administering Shoal and the other actions a permission key governs follow that
key. `config reload` needs no key, since it applies the config on disk and
stops nothing. A scoped caller reads the effective configuration of its own
workspace.

### The spaces below

A scoped caller manages the spaces below its own as the user would, since only
it knows whether their agents still have work. For example, it holds them,
stops, restarts and resumes their agents, converts and renames them, and
prioritizes or cancels their queue entries. It talks with them through alerts
and threads. [Spaces](spaces.md) says what a coordinator does and receives.

The daemon starts the agents such a caller queues or resumes detached, a
chat's in the place it ran in. `start --here` instead runs an agent in the
caller's terminal, under the scope of the agent's own space.

### Its own space

A scoped caller takes the actions that concern only its own space without
approval. Examples are running setup and demos, linking and watching items,
marking the space ready, taking checkpoints and completing its assignment.
Holding its own space is the exception, as [Holds](cleanup.md#holds) says. It
sends alerts and opens threads up, to its coordinator or the user.

It posts in the threads it takes part in, resolves those it opened and
forwards those it holds. It reads and links the threads of any space its scope
reaches.

### The forge

A scoped caller reads the issues, PRs and CI runs of any repository. It writes
to its own repository's: it opens and changes issues and PRs, and runs,
reruns and cancels CI. Its CI covers the default branch and the branches of
the workspaces its scope reaches. A chat without a repository counts every
registered repository as its own.

Writes that reach further follow a permission key: issues in other
repositories, forge API requests other than reads, and PR reviews. A review
goes out as the host's `review_token`.

### Reviews

A scoped caller runs, views, cancels and answers the reviews of its own
workspace and those below it. It writes its own reviews of those below it.
Where `[review]` requires it, opening a new PR, a ready mark and a merge wait
until a review covers the commit they act on.
[Review gates](reviews.md#review-gates) says when a review covers it.

## Approvals

An approval is the daemon's way for a scoped caller to ask for a yes or no. A
scoped caller requests approvals and reads its own. Unscoped callers decide
any request. [Spaces](spaces.md) says how a request goes up the tree to the
space that decides it.

A decision binds to the record its decider was shown or authorized, whoever
decides, the user or a coordinating agent. A request that took a reused
number meanwhile is never decided by mistake.

## Permissions

### Permissions belong to spaces

Permissions belong to spaces, not repositories. The user may trust a
top-level chat they talk with more than a worker several levels down.

A space's permissions come from its agent's mode. The parent then caps each
key, so a child gets no more from its mode than its parent has.
[Mode permissions](../reference/modes.md#mode-permissions) says how the values
combine.

### Built-in defaults

The key defaults propose the actions a parent can judge and deny the rest.
The user keeps an action by setting its key to `"propose"` or `"deny"`.

Modes that coordinate open and decide their children's issues in other
repositories, so they hold that key at `"direct"`. A master session does what
the user would, so it holds most keys at `"direct"`. It still proposes the
actions that stop or bypass every agent. The
[configuration reference](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#permissions)
lists each mode's defaults.

### Given permissions

`shoal start --permission KEY=VALUE` gives one agent a permission. The value
applies as given, whatever the mode or the parent allows. It is a decision
about that agent, not a default: the user may trust one worker to merge, or a
parent may give a child less than it has.

The giver bounds the value. A value is accepted only from one who holds it:
the user, who holds every permission, or a space's agent up to its own
effective value. The daemon checks this wherever a scoped process brings a
value in.

A given value is recorded with the agent's mode, since it belongs to that
agent as its mode does. Restores and restarts keep both, and a new agent in the
space brings its own. A restore or registration brings back only what the space
recorded. An agent thus cannot raise its permissions by replaying a record.

### Changing given permissions

`shoal resume` and `shoal restart` change the given values key by key. A parent
thus changes what a child may do without ending its session or restating the
rest. The mode stays, since a new mode would change the agent's rules
mid-session.

A dropped key falls back to the space's value without one. The giver must hold
that value too, so a drop cannot hand a child what a space between them was
given.

The change goes into the record that every restore reads. A wrapper that
restores its own agent, after an in-place restart or a protection stop, reads
it too, and rebuilds its push rewrites when they changed. The daemon claims
the record until the agent starts, so no change lands unseen.

### Computed at each check

Shoal computes effective permissions at each check, from the current config,
the recorded modes and given values, and the tree. It never stores them, so a
config change reaches running agents. Only the push rewrites wait for a
restart, since they are Git environment fixed at launch.

A check uses the acting space's permissions. A parent that acts on a child
acts under its own.

### Actions a key governs

What only the user could do is an action a permission key governs. Examples
are administering Shoal, stopping the daemon, reading the user's alerts and
deciding any request below. Scope answers which spaces an agent acts on. Trust
answers which actions it takes, and the user sets trust per mode.

With `"propose"`, the action becomes an approval request bound to what it
does. Approval lets the same action proceed once. An approval thus never
covers a later, different action.

A command sends several requests. Approving it grants a scope token that its
requests carry. The token takes that key's actions without asking again, for
an hour at most, and ends with the daemon.

### Deciding for others

Deciding is a key of its own, `decide`. A space that holds it decides any
request of a space below as the user would, with or without a grant, even one
routed to a space between them. It never decides its own requests. It approves
an action only where it holds that action's key at `"direct"`. A key a master
session proposes thus never passes through a child it starts.

### Where the checks run

The daemon checks every request it receives, so a protocol client cannot step
around the CLI. The CLI asks the daemon before what a command does itself, such
as writing the config. The two thus never disagree.

Config sets permissions, so a scoped process edits it key by key. It sets a
permission only to a value it holds, as it gives one with `--permission`.
Replacing the file could drop a narrowing, so that stays the user's. A setup's
agent edits the file, so a setup starts as `admin` allows.

Forge writes these keys allow go out as the agent account, so the forge shows
who made them.

## Proposals

### Why agents propose

In a shared repository, other people see everything an agent opens. The
`issue_open` and `pr_open` permissions can thus make a scoped `issue open` or
`pr open` propose instead of open. Unscoped callers always open. Other agent
writes, such as comments, edits and closing, stay direct.

### What a proposal records

A proposal is an approval request. Its record carries the command's options
with their defaults resolved, the workspace, branch, commit and agent. Copies
of its attachments wait in its repository directory until no pending proposal
uses them, and a move of that directory carries them over.

The command reports the proposal and exits 0. The agent goes on with its work
and hears the decision as a message. A decided proposal is deleted, since the
message carries the outcome.

### Who approves a proposal

A proposal goes up the tree like any request. Publishing to a forge other
people see is the review the setting asks for. A space above thus approves a
proposal only where its own permission for it is `"direct"`. It could then
publish the item itself, so reviewing a child's is within what it may do.

A space above without that permission denies the proposal, with a reason or as
a duplicate, or passes it up. A duplicate only adds a finding to an existing
issue or proposal, so it opens nothing.

The decider may replace the title and body. The daemon then opens the item as
the agent account, through the same path `issue open` and `pr open` use, and
records and links it.

### Issue proposals belong to the repository

An issue proposal belongs to the repository, not to the space that made it,
since agents file follow-ups just before `done`. Once the space is gone, the
issue opens through the repository's checkout and agent account.

### PR proposals

A PR proposal binds the branch and its commit, as a merge request does. The
push still happens unless the `push` permission holds it back. A renamed
branch or another commit, locally or where the push went, outdates the
proposal. Shoal checks this at approval and when the agent proposes again.

## Pushes

### Why pushes are held

A proposed PR still publishes its branch, and agents also push with plain Git.
A `push` permission of `"propose"` thus holds a scoped caller's pushes to the
workspace repository's remotes. `"deny"` refuses them, for repositories that
should get no agent push, and records nothing. Unscoped callers always push.

### What a push request binds

A push request binds exactly what the push changes: the remote, its push URL
where it has several, the ref and the object. A forced push or a deletion also
binds what it replaces. Approval thus cannot push something its decider did
not see. A newer push to the same ref outdates the request, and so does a
moved branch at approval.

A new PR whose push is held becomes one PR proposal that carries the push,
whatever `pr_open` says. The PR cannot open first, so one approval pushes and
opens.

### Plain `git push`

Shoal holds plain `git push` by rewriting the agent's push URLs, through its
Git environment, to a Shoal remote helper. The helper asks the daemon about
each ref.

Shoal chose the rewrite over a pre-push hook for two reasons. `--no-verify`
skips hooks, and a Shoal hook would replace the repository's own. Like scope,
the rewrite stops accidents, not a process that clears its Git settings.

A push URL that a fetch URL starts with cannot be rewritten without breaking
fetches. The agent's launch refuses it rather than leave those pushes unheld.

### Approval runs the push

Approval runs the push without the agent, with the agent's Git profile, as
`pr open`'s push does. The end of that operation decides the request. Nothing
decides, withdraws or replaces a request while its push runs, since the push
lands regardless. Pushes Shoal authorized itself run without the rewrites.

### Merges and reverts

A scoped `merge` is an approval request bound to the branch and its HEAD,
unless the caller's `merge` and `push` permissions are both `"direct"`. A
merge may push the default branch, so `push` governs it too. A scoped
`unmerge` is a request bound to the merge it undoes. Approval lets the agent's
retry merge that commit, or undo that merge, through the normal checks. It
thus replaces only the user typing the command.

A forge's `pr revert` opens its PR and pushes at once. A scoped caller thus
reverts through the forge only where both `pr_open` and `push` are
`"direct"`.

## Resource approvals

A configured resource may require approval for a scoped acquisition. The
daemon persists the request with the caller's reason and the effective
allocation settings. Approval binds those settings, so changed settings need a
new request. It never overrides capacity, scope or simulator cleanup rules,
and a pending request takes no capacity.

Resources are not permissions, so a resource grant is the only measure of what
a space may hand on. A space approves a request below it only where it holds an
approved grant with the same target and settings, or holds
[`decide`](#deciding-for-others). Otherwise it denies the request or passes it
up, and the user decides at the top.

The decision reaches the requesting workspace's agents as a message, so they
need not poll. Requests and grants survive a restart and a failed removal, and
go with a successful removal. A malformed record blocks only the lookups that
select it, never access through other records.
