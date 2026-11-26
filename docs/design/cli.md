# Command line

This file covers how the CLI groups commands, selects targets, prompts, prints,
completes and shows problems. [Workspaces](../reference/workspaces.md) says how
commands select a space and what `list`, `inbox` and `status` show.
[Output](../reference/output.md) says how commands print, and
[Installation](../reference/install.md#shell-integration) covers shell
integration and tab completion.

## Commands

Root help groups the commands by task. AGENTS.md's Command line rules say how
commands, flags and values are named.

Each command has one subject. `shoal resume` brings back a space's agent, in a
workspace or a chat, whatever state it is in. `shoal restore` reopens a removed
space or returns a workspace to a checkpoint. A restore resumes the reopened
space's agents through `resume`'s operation, in the same run, so the space comes
back in one command. `restore` refuses a space that is not removed and names
`resume`.

`shoal status` shows a workspace and a chat through one path. Both thus show
their links, watched PRs, completion, sessions and what waits for the user
alike. The daemon looks up the PRs on each request and keeps a failed lookup on
its PR, so one failure leaves the rest of the status.

## Targets

A command that acts on one space takes an explicit target, then the caller's
scoped space or the workspace that holds the current directory, then a picker.
[Select a space](../reference/workspaces.md#select-a-space) gives the details.

An invalid target or other refused argument fails without a fallback, and
before any prompt for the omitted ones. A picker thus never stands in for a
mistyped target, and nobody answers prompts for a command that fails. `start`
first sends the daemon its launch with what the pickers would choose still
unset. The daemon runs its start's steps up to where they would create or fetch
anything. Each input a start refuses is thus refused before a picker opens.

The daemon enforces scope. Pickers, completion and operations on every
workspace keep the caller's scope.

### Several targets

A command that acts on several spaces checks every target before it acts on
any, so one it cannot take leaves the others as they were. It asks one
confirmation that names what each space loses. Once it acts, it goes on after
one fails and exits 1.

### The last space

`-` selects from a history of space IDs per command. The CLI keeps it in the
state directory, shared across terminals, so `attach -` works from a new pane.
The history only saves typing: it never fails a command or authorizes
anything. Scoped callers neither read nor write it.

`attach`, and every launch that shows an agent session here, such as `start`,
records the space once the session was shown and still runs: when the terminal
detaches, or at once for a host that shows it without attaching the terminal.
`attach -` thus returns to the session last left, not to one another terminal
still shows or one that ended. A focused `start` records the workspace once its
place hosts it, since a host may show the place before the workspace exists.
[The last space](../reference/workspaces.md#the-last-space) says what each
command records.

## Pickers and completion

Pickers and completion judge a space by the request its command would send for
it. Each request kind declares the one check it makes of the space it names,
with no default, such as a ready workspace or one where an agent runs. The
daemon judges the space by that check before the request's handler runs. It
accepts the space, refuses it with the check's reason, or finds it without
what the request acts on, which the handler answers.

A picker names the request its command sends. The daemon of each machine
judges that machine's spaces, by facts only it may have, such as where its
terminal hosts keep agent sessions. The picker leaves out a space without what
the request acts on, and lists a space the request refuses last, with the
reason. A command that acts on this machine's spaces in its own process asks
only this machine's daemon.

Every command declares that request, or that it sends none a space's offer
depends on. The declaration is a match over the command tree with no default
arm, so a new command cannot leave it out. Pickers and completion read the same
declaration.

Completion asks this machine's daemon for the request of the line typed so far.
It offers every space while the line does not parse yet. Each Tab runs the
installed binary and reads the daemon without changing anything, under a short
limit. It never starts a daemon or a picker, so Tab stays fast and has no side
effects.

## Prompts

A prompt needs a person on stdin and stderr, and no JSON output. A flow that
shows details and then asks for a decision, such as a thread reply or an
approval, also needs stdout on a terminal. Redirected details thus never hide
what the user decides.

The CLI carries the key of each approval request it shows to the decision. A
number reused meanwhile thus cannot decide the request that replaced it, as
[Approvals](permissions.md#approvals) requires.

### Confirmations

A confirmation shows the action and asks `[y/N]`. Enter, `n`, end of input and
Ctrl-C cancel it, also after a tracked execution. Only an explicit flag, such
as `--yes`, skips it. A missed answer thus never destroys work.

A confirmation names what the action loses. The daemon refuses unsaved work the
confirmation did not cover, as
[Unsaved work](../reference/removal.md#unsaved-work) describes for removal.

### Offered fixes

When the fix for a failure is one command, the CLI asks the user `[y/N]` whether
to run it and continue. A refusal fails, naming the command. Without a terminal,
or with JSON output, the command fails the same way without asking.

## Output

Text output shows enum values through their `Display` and colors through a
shared palette of meanings, such as success. Only the CLI's presentation layer
applies them. JSON output and stored values stay plain.

The output format resolves once after parsing, so results, errors and prompts
agree. The format changes only what a command prints, and turns off prompts and
pickers with it. Where and how a command works follows whether it has a
terminal. [Format](../reference/output.md#format) gives the precedence.

The CLI builds every internal worker's command through one typed builder. It
passes the resolved state directory and output format explicitly, so the worker
reaches the caller's daemon and prints as the caller does. Hooks run without a
format.

Agent launches set `SHOAL_OUTPUT=json` unless the launcher carries a setting.
Shoal does not guess the format from the workspace or the terminal: people use
workspaces too, agents sometimes have terminals, and piped text output keeps
its meaning.

Progress during a silent wait belongs to the CLI, as
[Progress](../reference/output.md#progress) describes.

## The picker of what needs the user

Bare `shoal` is the one place to see what needs the user and the work going on.
It is one picker with three views: the inbox, the spaces and recent alerts.
`inbox` and `list` open it on their own view.
[List](../reference/workspaces.md#list) and
[Inbox](../reference/workspaces.md#inbox) say what each view shows.

Each view binds only the keys that act on its rows. A key means the same in
every view that binds it, and the header lists the keys by how often each is
used. Acting on a row runs the command that handles its kind.

The inbox lists only what needs the user. Items that only report status stay
out. An idle agent is one, since it may wait for PR activity rather than the
user.

## Problems

Shoal has no diagnostic command. Each problem shows where the user looks and
names the command that resolves it:

- a failing command's error says what to run;
- the inbox lists failed workspaces and untracked worktrees;
- the daemon checks its environment when it starts or reloads its config, and
  records an alert for each problem.

Repair is a request the commands that need it send, such as `adopt` and
`setup --skip`. Every command reports an unreachable or mismatched daemon
itself. [Recovery](../reference/daemon.md#recovery) gives the checks and the
repairs.

## Shell integration

The Rust binary chooses every path the shell moves to, such as
`<root_dir>/<repo>` after a removal. The Bash and Zsh wrapper only changes
directory, and never evaluates repository code.

`shoal done` does not move the shell. Cleanup removes the worktree later, and
the shell moves once it is gone. When cleanup removes the shell's directory or
a pending destination, the shell moves to the nearest directory above that
still exists, after the command or at the next prompt. This recovery needs no
daemon and is off for scoped callers.
[Navigation](../reference/install.md#navigation) gives the details.
