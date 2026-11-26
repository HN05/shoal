# Repositories

This file says what the `repo` commands do: registering, contributing to,
deleting and moving repositories, and Git profiles.

Other files cover the other `repo` commands: [Issues](issues.md#repository-conventions)
covers `repo labels`, [Configuration](config.md) covers `repo setup` and
`repo config`, and [Hub](hub.md#shared-repositories) covers sharing
repositories between machines.

## Register a repository

```sh
shoal repo add /path/to/project --name my-project   # A local checkout, in place
shoal repo add https://github.com/owner/repo         # Clone a URL
shoal repo add https://github.com/owner/repo --path ~/src/repo
shoal repo ls
shoal repo rename my-project web
```

`repo add` registers a local checkout in place or clones a URL. A local
checkout needs no remote. Registering never fetches.

Registering is idempotent. A repository matches an existing registration by
its normalized `origin` URL, where HTTPS and SSH forms and a `.git` suffix
match, or else by its canonical path. Registering a URL again with its existing
path succeeds, and with another path is refused. Shoal drops a clone URL's
trailing slashes, so forge CLIs recognize the worktrees' remote.

`repo ls` lists the registered repositories. `repo rename` renames one, and
asks for the new name when you omit it.

### Repository directories

Registering gives each repository a directory `<name>/` in
[`machine.root_dir`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine),
which is `~/shoal` by default. That link says what a later change of
`root_dir` does to it. Its
[workspaces](workspaces.md#worktree-directories) go there by default, and a
URL clone lives there as `.checkout`.

The name comes from `--name`, else from the source's basename without `.git`.
When a file or a recorded path already has that name, the directory takes
`-2`, `-3` and so on. A local checkout already at `~/shoal/<name>/<anything>`
keeps that directory.

`repo add <url> --path <dir>` clones the repository into an exact new
directory instead. A relative path starts from the current directory, and `~/`
is allowed.

Shoal resolves symlinks and `..` in `root_dir` before it checks and creates the
directory, even when components are missing.

Registering, and creating a workspace, fail when the daemon cannot create a
file in the directory within 30 seconds. This happens on a volume macOS has not
given the daemon access to.

### Spotlight on macOS

On macOS, Spotlight does not index a `root_dir` Shoal creates. The `root_dir`
is a symlink to `<parent>/.<name>.noindex`, a directory Spotlight skips by its
name. `~/shoal` still leads to every workspace, while Shoal and Git show the
real paths.

Shoal uses an existing `root_dir` as it is.
[`repo move`](#move-repositories-to-a-new-root) moves it out of Spotlight, or
back with `--regular-dir`. The [repositories design](../design/repositories.md)
says why.

## Select a repository

A command that takes a registered repository selects it by its ID, its source
URL, its path or its name. A command that omits it uses the repository of the
current registered checkout or workspace, else one a terminal picks.

- A name from `--name` or `repo rename` is unique. A name Shoal inferred from
  the source selects the repository only when no other has it.
- IDs and sources take precedence over paths.
- A name still selects its repository when a path in the current directory has
  the same spelling. When that path is another registered checkout, the
  selector is ambiguous, and the error lists both matches.

## Repository menu

Bare `shoal repo` on a terminal opens a list of the registered repositories,
built as [`shoal list`](workspaces.md#list) is. Its keys:

- Enter: start an agent in the repository, as `shoal start <repository>` does.
- Ctrl-A: register a path or URL.
- Ctrl-R: rename the repository.
- Ctrl-O: show its [local config](config.md#store-repository-config-outside-git).
- Ctrl-S: run [`repo setup`](config.md#repository-config).
- Ctrl-D: delete the repository.

Without a terminal, bare `shoal repo` prints the `repo` help.

## Default branch sync

```sh
shoal repo sync              # The current repository
shoal repo sync my-project
```

The daemon keeps each registered checkout's default branch in sync with its
remote. It syncs when it starts, every minute, and right after `shoal merge`
merges a PR or a watch sees a linked PR merge. `repo sync [repository]` syncs
at once and reports the result or the error.

A sync fetches the default branch's upstream remote and the default remote.
It then fast-forwards the local default branch. When the checkout has another
branch checked out, the sync moves only the ref.

A sync leaves the local default branch unchanged when:

- the branch is ahead of its remote;
- it has no upstream, or the fetch fails;
- it has diverged from its remote;
- it is checked out with uncommitted changes, or checked out in a workspace.

[`shoal status`](workspaces.md#status) shows a workspace's last sync and its
error. Once syncs have failed for 30 minutes of the machine being awake, the
repository joins the [inbox](workspaces.md#inbox), and one `sync_failed` alert
tells you.

A sync never pushes and never moves a workspace's branch.
[`shoal refresh`](branches.md#refresh) fast-forwards one when you ask.

## Contributions

```sh
shoal contribute https://github.com/owner/repo/issues/68  # Work on an issue
shoal contribute https://github.com/owner/repo --branch fix-typo --agent claude
```

`shoal contribute <url>` creates a workspace in a repository you do not
maintain. The URL names the repository, or an issue, PR or branch in it. A link
opens as `shoal start <link>` does, and `--branch` opens or creates a branch as
`start`'s does. Without a link or a branch, the contribution begins as a chat,
as a [start without one](workspaces.md#branches) does. The other flags work as
they do for `start`.

`contribute` uses a registered repository as it is. Otherwise Shoal clones it
as a contribution, after checking whether your forge login can push to it.
When it cannot, Shoal forks the repository into your account, or reuses an
existing fork. The clone gets the fork as its `fork` remote, with
`remote.pushDefault` set to it. [`pr open`](issues.md#open-a-pr)
then pushes there and opens the PR from the fork. Forking needs GitHub,
Forgejo or Gitea.

### Remove a contribution

The cleanup sweep removes a contribution as `repo rm` does, once nothing of it
has been left for
[`[cleanup] grace_minutes`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup).
Something is left of it while it has:

- a running `shoal contribute`;
- a workspace, a queued agent, a chat or a pending issue proposal;
- a kept branch, or a resource another repository's workspace borrows;
- an unexpired [checkpoint](removal.md#checkpoints), removal
  record or removed space's [files](files.md);
- changed files, a stash or a Git operation under way in its checkout;
- commits in its checkout that no branch or tag its remotes advertise has.

From the start of the removal, Shoal refuses new workspaces, queued agents and
chats in it. Once it is removed, a `repository_removed` alert tells you, and
names your fork when the clone pushed to one.

The fork stays on the forge, where a later `contribute` reuses it. Delete it
there when you no longer need it.

## Git profiles

```sh
shoal start my-project --git-profile work
```

[`[machine.git_profiles]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#git-profiles)
names sets of Git settings. A top-level `git_profile` in repository or global
config selects one for new worktrees. `shoal start --git-profile` overrides it
for one creation. Without a selection, Shoal leaves Git's settings unchanged.

Shoal refuses an unknown `--git-profile` before it creates anything. An unknown
configured profile leaves a failed workspace, to inspect or remove.

Shoal writes the profile's settings into Git's per-worktree config before setup
runs. Other worktrees keep their settings. Reopening or preparing a workspace
does not apply them again, and a reopen refuses `--git-profile`.

Shoal enables Git's shared `extensions.worktreeConfig` setting when needed. It
refuses a repository whose shared config sets `core.worktree` or
`core.bare = true`. Move those settings to the main checkout's
`config.worktree` first.

## Delete a repository

```sh
shoal repo rm my-project -y
shoal repo rm my-project --machine desk   # On the joined machine desk
shoal repo rm my-project --all            # On every machine that registers it
```

`repo rm` permanently deletes a repository. It first stops the managed commands
and agents in it, then deletes:

- the checkout, even an in-place local one with uncommitted or unpushed work;
- every workspace and chat, with their ports, simulators and leases;
- the seed, the shared caches and the local config.

A workspace still being prepared goes too, once the creation of its worktree
ends. A `shoal start --wait` that waits for its preparation then fails, naming
the repository. A workspace whose setup runs refuses the removal until setup
ends.

Shoal tries to delete the workspaces' branches. It keeps a branch whose
recorded worktree directory is already missing, since it cannot inspect what
the branch holds.

### Other machines

`repo rm` finds a named repository wherever it is registered, as a named space
is found:

- One registered here is removed here.
- One registered only on one other joined machine is removed there.
- One registered on several other machines, but not here, opens a picker on a
  terminal. Without a terminal, the command is refused and asks for
  `--machine` or `--all`.

Without a name, outside a checkout, the picker lists the other machines'
repositories after this machine's.

`--machine NAME|ID`, repeatable, removes the repository on the named machines,
this one included. `-A`/`--all` removes it on every machine that registers it.
Each machine removes its own registration of the name, else of the same origin.

When a machine it would act on is offline, the removal is refused at once,
naming that machine, before anything is removed. The hub never holds a removal
for later.

### Confirmation

On a terminal, `repo rm` lists for each machine the resolved repository, its
source and checkout path, and the spaces it deletes there. Each workspace shows
its uncommitted files and unpushed commits. Then it asks
`Are you sure? [y/N]`.

`-y`/`--yes` skips the prompt. Scripts and `--json` need it. `--json` prints one
entry per machine, and names each other machine as `machine`.

### Refusals and failures

Remove linked worktrees outside Shoal first. Locked worktrees block the
removal, and prunable stale records do not.

Shoal refuses:

- redirected paths;
- a checkout whose Git config sets `core.bare` although it has a working tree;
- a deletion that would include another registered repository or Shoal's own
  state.

When a removal fails, the steps it completed stay done and the remaining
records stay. The repository refuses new workspaces until you run the same
command again.

## Move repositories to a new root

```sh
shoal repo move /Volumes/Work/shoal -y  # Save root_dir, then move
shoal repo move -y                      # Move into the configured root_dir
shoal repo move --regular-dir -y        # Keep root_dir a regular directory
```

`repo move` moves each repository directory outside `root_dir` into it. A
directory moves whole, with its URL clone, workspaces, seed and shared caches.
It keeps its name, or takes a suffix when the name is taken. A root on another
filesystem is copied, then the original is deleted. A checkout or workspace
placed outside the repository directory stays where it is.

A path argument is saved as the global `root_dir`, and the daemon reloads it
before the move.

On macOS, the move puts the repositories into the
[directory Spotlight skips](#spotlight-on-macos) and makes `root_dir` the link
to it. It refuses when `root_dir` holds other files. `--regular-dir` keeps
`root_dir` a regular directory: it replaces that link and moves the
repositories back.

### Confirmation and agents

On a terminal, `repo move` runs [its checks](#checks-before-a-move), then lists
the directories to move, the chats whose files move, and the commands and
agents running in them. Then it asks. `-y`/`--yes` skips the prompt. Scripts
and `--json` need it. `--json` returns each moved `repository`, its old
directory as `from` and its old `workspace_paths`.

The move stops those commands and agents as [`shoal stop`](load.md#shoal-stop)
does. Once the move is done, or has failed, it queues their agents' restores
as `shoal resume` does. The agents continue their sessions in the new paths. A
workspace's stopped commands go to its restored agent. Without one, they stay
for `shoal resume`.

[Trust entries](agents.md#launchers-and-trust) for the old paths move to the
new ones.

### Checks before a move

Shoal checks every repository before the first one moves. The move is refused
while a repository directory has:

- a running chat, queued agent launches or a starting CI job;
- a queued or running review, or an import reading a package into it;
- a workspace in another lifecycle operation;
- a checkout leased as a repository resource;
- another repository's checkout or workspace, or Shoal's state;
- a process with its working directory inside it, other than the commands and
  agents the move stops. Leave those directories first.

A Git command the daemon itself runs there counts only after Shoal has waited
up to a minute for it.

### Chats without a repository

The files of a [master session](chats.md#master-session) and of any other chat
without a repository move with the repositories into the new root. They are
its temporary directory, attached files and kept imported session files. They
move whether the chat is running, stopped or removed, and the chat runs from
the new root from then on. A running chat stops and restores as the agents do.

The move deletes the `.tmp`, `.attachments`, `.sessions` and `.imports` these
chats leave in the old root. It refuses while an import reads a package into
that root.

### Interrupted moves

Shoal records the move before anything moves. A failed or interrupted move
rolls back, or finishes once the directory has arrived. Until the next
`repo move` or daemon start completes it, the repository refuses other
changes.
