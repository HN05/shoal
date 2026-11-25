# Repositories

This file says what the `repo` commands do: registering, contributing to,
deleting and moving repositories, and Git profiles.

Register a local checkout in place (no remote required) or a clone URL. Each
repository gets `~/shoal/<name>/`, named by `--name` or the source basename without
`.git`, suffixed `-2`, `-3` on conflict with files or recorded paths; its workspaces
default to it and a URL clone lives there as `.checkout`. A local checkout
already at `~/shoal/<name>/<anything>` keeps that directory.
[`machine.root_dir`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine) changes the parent for new
registrations; symlinks and `..` are resolved before checking and creating the root,
even across missing components. Existing registrations keep their paths until
[`repo move`](#move-repositories-to-a-new-root) moves them, as do workspaces
created before this layout. `repo add <url> --path <dir>` clones one repository to an exact new
directory (relative to the current directory; `~/` allowed). Re-registering a URL
with its existing path is fine; a different path is rejected. Registering, and
creating a workspace, fail when the daemon cannot create a file in the directory
it would write within 30 seconds, as on a volume macOS has not given it access to.

On macOS, Spotlight does not index a `root_dir` Shoal creates: it is a symlink
to `<parent>/.<name>.noindex`, a directory Spotlight skips by name, so `~/shoal`
still leads to every workspace while Shoal and Git show the real paths. An
existing `root_dir` is used as it is; [`repo move`](#move-repositories-to-a-new-root)
moves it out of Spotlight, or back with `--regular-dir`.

Registration is idempotent by normalized `origin` URL (HTTPS/SSH forms and
`.git` suffixes match) or canonical local path, and never fetches. Clone URLs
lose trailing slashes so worktrees get a remote forge CLIs recognize. Names from
`--name` or `repo rename` are unique and work as selectors alongside IDs, paths,
and source URLs; inferred names work when unambiguous. IDs and sources take
precedence over paths. A name still resolves when
an unrelated path has the same spelling in the caller's directory; if that path
is another registered checkout, the selector is ambiguous and lists both matches.
A command that takes a registered repository and omits it uses the repository
of the current registered checkout or workspace, else one a terminal picks.
`repo rename` asks for an omitted name.

Bare `shoal repo` on a terminal opens a list of the registered repositories,
built as [`shoal list`](workspaces.md#list) is. Enter adds a workspace, Ctrl-A
registers a path or URL, Ctrl-R renames, Ctrl-O shows the local config and
Ctrl-D deletes. Without a terminal it prints the `repo` help.

The daemon keeps each registered checkout's default branch in sync with its
remote: when it starts, every minute, and right after `shoal merge` merges a PR or a
watch sees a linked PR merge, it fetches the default branch's upstream and default remotes and fast-forwards the local default branch, moving
only the ref when the checkout is on another branch. An ahead branch stays as it
is; a missing upstream, failed fetch, divergence, or a dirty or managed checkout
of the default branch leaves it unchanged until fixed. Workspace status shows the
last sync and its error; once syncs have failed for 30 minutes of the machine
being awake, the repository joins the inbox and one `sync_failed` alert tells you. `shoal repo sync [repository]` syncs at once and
reports the result or the error. Syncing never pushes or moves workspace
branches; [`shoal refresh`](branches.md#refresh) fast-forwards one when asked.

## Contributions

```sh
shoal contribute https://github.com/owner/repo/issues/68  # Work on an issue
shoal contribute https://github.com/owner/repo --branch fix-typo --agent claude
```

`shoal contribute <url> [--branch <name>]` creates a workspace in a repository you do not
maintain. The URL names the repository, or an issue, PR or branch in it, which
opens as `shoal start <link>` does; `--agent`, `--here`, `--focus` and `--now` work
as for `start`, `--branch` opens or creates its branch as `start`'s does, and
without a branch or link the contribution begins as a chat,
as a [start without one](workspaces.md#branches) does. A repository that is already registered is used as it is.
Otherwise Shoal clones it as a contribution, first checking whether your forge
login can push to it. When it cannot, Shoal forks it into your account, reusing
an existing fork, and the clone gets the fork as its `fork` remote with
`remote.pushDefault` set to it, so [`pr open`](issues.md#issue-and-pr-actions) pushes
there and opens the PR from the fork. Forking needs GitHub, Forgejo or Gitea.

The cleanup sweep removes a contribution repository as `repo rm` does once it has
had no running `shoal contribute`, workspace, queued agent, chat, pending issue
proposal, kept branch or resource another repository's workspace borrows, no unexpired
[checkpoint](removal.md#checkpoints-and-restore), removal record or removed space's
[files](files.md), and no changed files,
stash, Git operation under way or commits in its checkout that no branch or tag
its remotes advertise has, for
`[cleanup] grace_minutes`, then records a `repository_removed` alert,
which names your fork when the clone pushed to one. The fork stays on the forge,
where a later `contribute` reuses it; delete it there when you no longer need it.
From the start of the removal, new workspaces, queued agents and chats in it are
refused.

## Git profiles

[`[machine.git_profiles]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#git-profiles) names Git settings, and a
top-level `git_profile` in repository or global config selects one for new
worktrees. Override it for one creation with `shoal start <repo> --git-profile work`.
With no selection, Git settings stay unchanged. An unknown flag value is rejected before creation; an unknown
configured profile leaves a failed workspace for inspection or removal.

Settings go into Git's per-worktree config before setup runs; reopening or
preparing a workspace does not reapply them, and an explicit profile flag on
reopen is rejected. Shoal enables the shared
`extensions.worktreeConfig` setting when necessary, refusing repositories with
shared `core.worktree` or `core.bare = true` until those settings are migrated
to the main checkout's `config.worktree`. Other worktrees retain their settings.

## Delete a repository

```sh
shoal repo rm my-project -y
shoal repo rm my-project --machine desk   # On the joined machine desk
shoal repo rm my-project --all            # On every machine that registers it
```

Permanently deletes the checkout (including in-place local repositories with
uncommitted or unpushed work), all its workspaces and chats, their ports, simulators, and
leases, its seed and shared caches, and its local config, stopping managed commands and agents first. A workspace
still being prepared goes too, once the creation of its worktree under way ends, and a
`shoal start --wait` following its preparation fails naming the repository; one whose setup runs
refuses the removal until setup ends. Shoal attempts to
delete workspace branches; a branch whose recorded worktree directory is already
missing is retained because its contents cannot be inspected.
A named repository is found wherever it is registered, as a named space is:
one registered here is removed here, and one registered only on one other
joined machine is removed there. One registered on several other machines but
not here opens a picker on a terminal, and is otherwise refused asking for
`--machine` or `--all`; without a name, outside a checkout, the picker lists
the other machines' repositories after this machine's. `--machine NAME|ID`,
repeatable, removes it on the named machines, this one included, and `-A`/`--all`
on every machine that registers it, each machine by its registration of the
name, else of the same origin. A machine it would act on that is offline
refuses the removal at once, naming it, before anything is removed; the hub
never holds a removal for later. Interactive calls list, for each machine,
the resolved repository, its source and checkout path, and the spaces removal
deletes there with each workspace's uncommitted files and unpushed commits,
before asking `Are you sure? [y/N]`; `-y`/`--yes` skips the prompt and is
required for scripts and `--json`, which prints one entry per machine, naming
each other machine as `machine`. Removing a shared repository anywhere stops
sharing it, so every other machine keeps its registration, no longer shared.
Linked worktrees outside Shoal must be removed first; prunable stale records do
not block, locked worktrees do. Shoal refuses redirected paths, a checkout whose
Git config sets `core.bare` although it has a working tree, and deletions
that would include another registered repository or its own state. If cleanup
fails, completed steps stay done, remaining records are retained, and new
workspace creation is blocked until the same command is retried.

## Move repositories to a new root

```sh
shoal repo move /Volumes/Work/shoal -y  # Save root_dir, then move
shoal repo move -y                      # Move into the configured root_dir
shoal repo move --regular-dir -y        # Keep root_dir a regular directory
```

Moves each repository directory outside `root_dir` into it whole, with its URL
clone, workspaces, seed and shared caches, keeping its name or suffixing it when
taken. On macOS it moves them into the [directory Spotlight skips](repo.md)
and then makes `root_dir` the link to it, refusing when `root_dir` holds other
files; `--regular-dir` keeps `root_dir` a regular directory, replacing that link
and moving the repositories back. A root on another filesystem is copied, then the original is deleted. A
checkout or workspace placed outside the repository directory stays where it is.
A path argument is saved as the global `root_dir` and the daemon reloads it first.
Interactive calls run the checks below, then list the directories to move, the
chats whose files move and the commands and agents running in them, and ask; `-y`/`--yes` skips the prompt
and is required for scripts and `--json`, which returns each moved
`repository`, its old directory as `from` and its old `workspace_paths`.
Those commands and agents stop as [`shoal stop`](daemon.md#recovery) stops them,
and once the move is done, or has failed, their agents' restores queue as
`shoal resume` queues them, continuing their sessions in the new paths; a
workspace's stopped commands go to its restored agent, or without one stay
for `shoal resume`.

Every repository is checked before the first moves: none may have a running
chat, queued agent launches or a starting CI job, a queued or running review, an import reading a package into it, a workspace in another lifecycle
operation, a checkout leased as a repository resource, another repository's
checkout or workspace or Shoal's state inside its directory, or a process
other than those commands and agents whose working directory is inside it, so
leave those directories first. A Git command the daemon itself runs there is
waited for, for up to a minute, before it counts. The temporary directory, attached files and kept imported session files of a [master session](chats.md#master-session)
or any other chat without a repository, running, stopped or removed, move with
the repositories into the new root, where the chat runs from then on; a running
one stops and restores as their agents do, and the move deletes the `.tmp`,
`.attachments`, `.sessions` and `.imports` left in the root they left,
refusing while an import reads a package into that root. The move is recorded before anything moves; a failed or
interrupted move rolls back, or finishes once the directory has arrived, and the
repository refuses other changes until the next `repo move` or daemon start
completes it. Agent trust entries for the old paths move to the new ones.
