# Distribution

This file covers how Shoal is released, signed and installed, how running
agents reach a moved installation, and how agents keep macOS privacy grants.
[Installation](../reference/install.md) says how to install and update Shoal,
and [releases](../releases.md) how to make a release.

## Packages

Each release holds prebuilt binaries for Linux and macOS, on x86_64 and arm64.
It publishes each platform's binary bare, since a single file needs no archive.
On macOS, a signed `Shoal.app` bundle also holds the binary. A bundle is a
directory, so it travels as an archive.

One table of release targets drives the cross-builds, the asset names and the
Homebrew formula's platform selectors. A new target thus needs one entry.

## Package managers

Shoal generates its Homebrew formula, which lives in the shared HN05 tap. The
formula installs the bare binary on Linux and `Shoal.app` on macOS. It has no
head channel, so every Homebrew installation runs a release that CI signed.

The formula links `bin/shoal` to the executable itself, so Homebrew launches
the binary directly, without a shell.

`shoal update` replaces a downloaded binary or bundle with a GitHub release. It
leaves a Homebrew installation to Homebrew, so one package manager owns each
installation.

## Moving an installation

An installation can move, such as from Homebrew to `~/.local/bin`, while
agents run. Each start of the daemon points the link `<state>/bin/shoal` at
the path it started through. The commands an agent's environment keeps for
its whole run, such as its Git credential helper, its push helper and its
secret tool launchers, run Shoal through that link. They thus reach the
moved installation once the daemon runs from it, without restarting the
agent.

## Development builds

A debug build is a development build. It shares the user's home with the
installed Shoal, so it leaves the user's own files to that Shoal:

- It refuses the user's state directory, however that is selected, since its
  daemon serves every running agent.
- It refuses to write the global config and the files beside it in the user's
  config directory, since running agents read their settings there.
- It changes no other user-level files outside its state directory, such as
  installed skills.

Tests in a temporary home set `SHOAL_DEV_USER_FILES=1` to lift these limits.
Unit tests refuse the user's config directory whatever they inherit. The
password database names the user's home, so a temporary `HOME` neither hides
nor imitates it. [Development](../../README.md#development) says how to run a
development build.

## The installed config

Shoal embeds its configuration templates in the binary at build time.
Installing thus needs no source checkout or network access.

The global config that installing writes sets nothing. It shows every setting
commented out with its default. An installed file never changes, so a live
value would pin the default of the release that wrote it. A later release that
renamed or removed that key would then break the file. `shoal config set` adds
a setting above the comments, which stay as its reference.

## Releases

One Forgejo Actions workflow makes a release, from the version bump to the
Homebrew taps.

### Versions and tags

The workflow bumps the version in a PR and merges it under normal branch
protection. It tags the exact merged commit, and attaches the cross-built
binaries from that tag. It never tags a later commit, downgrades or
force-merges. Selecting the version already tagged resumes from that tag, so an
interrupted release finishes without another version.

### Checks

A release runs no tests of its own. Its version commit changes only versions,
so the workflow accepts it once the `rust` and `musl` checks passed on the
`main` commit below it. It checks the version change with fmt and Clippy before
it opens the PR, and again only when the merge changed that tree. CI runs no
checks on the release PR or on its version commit on `main`.

### Release notes

The notes list the PRs merged since the nearest earlier published release the
tag contains, leaving out release preparation, and link their issues. They link
the runtime dependencies in the tagged README on each host.

The workflow recreates the release on GitHub from the tag, with a comparison
link. The GitHub notes leave out Forgejo's PR and issue references, since Git
mirrors do not carry them. A rerun brings the notes up to date and keeps an
asset set that is complete.

### Taps

The workflow updates both taps from each host's published checksums. A tap
update retries on the tap's latest `main` and never force-pushes.

## macOS signing

macOS keeps privacy grants, such as Removable Volumes, for an app bundle by its
identifier and designated requirement, wherever the bundle is installed. For a
bare executable, it keeps them by path. Homebrew installs each version at its
own path, so a bare executable would ask again after each upgrade. An ad-hoc
signature's requirement is its hash, so each release would need a new grant.

The release job therefore signs each macOS binary with `rcodesign` as
`com.shoal.cli`, with a project-owned self-signed certificate. It packages a
copy as `Shoal.app` and signs the bundle too. It fails unless both signatures
carry the expected designated requirement. The formula links `bin/shoal` to the
bundle's executable, so the daemon, the execution wrappers and every command
share the bundle's grant.

Grants need only a stable requirement, not Gatekeeper's trust. Shoal therefore
has no Developer ID and is not notarized. A new certificate changes the
requirement, so every user grants access again.

Nothing goes beside or inside the bundle, since a file added inside it breaks
its signature. The skills Shoal installs come from its binary.

## Privacy grants of agents

### Responsibility

macOS asks the process responsible for an access for its grant, such as for a
removable volume. A process inherits its responsible process from whatever
started it. A zmx session's processes inherit one that has exited. macOS then
judges each new program by itself, so a restarted agent asks for its own
grant, such as Claude Code's.

An execution wrapper started in a place that a host or the daemon opened
therefore claims responsibility. At startup, it replaces itself with its own
release, started with responsibility disclaimed. While the wrapper runs, the
agent and everything it starts use Shoal's grant, whatever started the wrapper.

Only certificate-signed builds claim. An ad-hoc build's grant lasts one build,
so test builds would prompt. A process that replaces itself with another
program, such as `shoal internal launch`, never claims, since that program would
take the responsibility over.

### Release copies

A wrapper replaces itself with a copy of its bundle, not the bundle itself.
Homebrew removes the previous version seconds after an upgrade. macOS then
denies the accesses of the agents whose wrappers still run it.

The copy lives at `<state>/releases/<version>-<hash>/`, cloned where the
filesystem allows. `<hash>` comes from the main program's SHA-256, so two
builds of one version never share a copy. Grants follow the bundle's
identifier and requirement, so the copy uses the existing grant.

A copy that fails leaves the wrapper running its release, with a warning. A
bare binary, whose grants follow its path, runs as it is.

### Removing copies

Each wrapper holds a shared lock on its copy's `lock` file, beside the bundle.
The cleanup sweep removes a copy it can lock exclusively, except in these
cases:

- It keeps the copy of the release the daemon was started through, which the
  next wrapper runs. The current release thus keeps one copy instead of a new
  one after each idle spell.
- It keeps a copy that a process runs a program in, such as one a wrapper
  started from its copy.

### Upgrades

The wrapper hands the copy the path it was started through and that file's
identity. The copy thus still finds the installed release and sees an upgrade.

The agent's hooks run Shoal through that path, even after an upgrade replaced
the file there. They thus reach the release the daemon restarts into. This
holds too for an agent restored in place while the daemon still ran the
earlier release.

A wrapper running a copy can continue from the installed release after an
upgrade, as an in-place restore does. It then replaces itself with a copy of
that release. It keeps the lock on the copy it started from, since macOS may
go on judging the process by the program it started with.
