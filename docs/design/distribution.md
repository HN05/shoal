# Distribution

This file covers releases, signing, installation and the macOS privacy grants
agents keep.

Distribution is a Homebrew formula in the shared HN05 tap: releases install
checksummed prebuilt Linux and macOS binaries on x86_64 and arm64. The formula
has no head channel, so every Homebrew installation runs a CI-signed release.
`shoal update` replaces a downloaded binary or app bundle with the latest GitHub release and
leaves Homebrew installations to Homebrew, so one package manager owns each
installation. A shared release-target table drives cross-builds,
asset names and Homebrew platform selectors.
Named configuration templates in `crates/shoal-core/configs/` are embedded at build time, so
installation needs no source checkout or network access.
The installed global config sets nothing: it shows every setting commented
out with its default, since an installed file never changes, so a live value
would pin the default of the release that wrote it and break the file once a
later release renamed or removed that key. `shoal config set` adds a setting
above the comments, which stay as its reference.
Shoal owns the generated formula. One Forgejo Actions workflow releases: it
bumps versions, validates, creates and merges a version PR under normal branch
protection, tags the exact merged commit, publishes merged-PR notes excluding release
preparation since the nearest earlier published ancestor release with issue links, and from
that tag attaches cross-built binaries and recreates the release on GitHub with
change descriptions and a GitHub comparison link, omitting Forgejo PR and issue
references that Git mirrors do not carry, then updates both taps from their
respective host's published checksums. Release notes link to the runtime
dependencies in the tagged README on each host. Reruns synchronize those notes while
preserving complete asset sets. It never tags a later commit, downgrades, or force merges;
explicitly selecting the current tagged version resumes from that tag, and tap
updates retry against the tap's latest main without force pushes. Deleted release
branch labels are accepted only for matching repository and exact PR identity.
A release runs no tests of its own: the version commit changes only versions, so
it accepts one once the `rust` and `musl` checks passed on the `main` commit
below it, and checks the version change with fmt and Clippy, before opening the
PR and again only when the merge changed that tree. CI runs no checks on the
release PR or on its version commit on main.

Release signing: the release job signs each macOS binary with `rcodesign` as
`com.shoal.cli` using a project-owned self-signed certificate, packages a copy
as `Shoal.app` and signs the bundle too, and fails unless both signatures have
the expected designated requirement. macOS keeps privacy grants such as Removable Volumes
for an app bundle by its identifier and that requirement, wherever it is
installed. For a bare executable it keeps them by path, and Homebrew installs
each version at its own path, so each upgrade would ask again; an ad-hoc
signature's requirement is its hash, so each release would need a new grant.
The formula links `bin/shoal` to the bundle's executable, so the daemon, the
execution wrappers and every command share the bundle's grant. A release
publishes each platform's binary bare, which downloaded installations and
Homebrew on Linux install, since a single file needs no archive. A bundle is a
directory, so macOS's `Shoal.app` alone travels as a `-app.tar.gz` archive,
which Homebrew, the hub installer and `shoal update` of a bundle install on
macOS. Nothing is added beside or inside the bundle, since a file added inside
it breaks its signature, and the skills Shoal installs come from its binary.
Only a stable
requirement is needed, not Gatekeeper trust, so there is no Developer ID or
notarization. The certificate came from `rcodesign
generate-self-signed-certificate --person-name "Shoal" --validity-days 7300`;
its unified PEM is the `MACOS_SIGNING_PEM` Actions secret, and the maintainer
keeps a copy outside Forgejo. A new certificate changes the requirement, so
every user grants access again.

Privacy grants of agents: macOS asks the process responsible for an access,
which a process inherits from whatever started it, for grants such as removable
volume access. A zmx session's processes inherit one that has exited, so macOS
judges each new program by itself and a restarted agent asks for its own grant
(Claude Code's). Execution wrappers started in a place a host or the daemon
opened (`internal place`, `session`, `detached` and `demo`) therefore replace
themselves at startup with their own binary, started with responsibility
disclaimed: the agent and everything it starts use Shoal's grant while the
wrapper runs, whatever started it. Only certificate-signed builds claim, since
an ad-hoc build's grant lasts one build and test builds would prompt. A process
that replaces itself with another program, such as `internal launch` or
`chat`, never claims: that program would take the responsibility over.
A wrapper replaces itself with a copy of its bundle rather than the bundle
itself: Homebrew removes the previous version seconds after an upgrade, and
macOS then denies the accesses of the agents whose wrappers still run it. The
copy lives at `<state>/releases/<version>-<hash>/`, cloned where the
filesystem allows, with `<hash>` from the main program's SHA-256, so two
builds of one version never share one; since grants follow the bundle's
identifier and requirement, it uses the existing grant. Each wrapper holds a
shared lock on the copy's `lock` file, beside the bundle, and the cleanup sweep
removes a copy it can lock exclusively, unless it is the copy of the release
the daemon was started through, which the next wrapper runs, or a process,
such as one a wrapper started from its copy, runs a program in it, so the
current release keeps one copy instead of a new one after each idle spell. The
wrapper hands the copy the path it was
started through and that file's identity, so it still finds the installed
release and sees an upgrade. The agent's hooks run Shoal through that path,
even after an upgrade replaced the file there, so they reach the release the
daemon restarts into, also for an agent restored in place while the daemon
still ran the earlier release. A copy that fails leaves the wrapper running its
release, with a warning; a bare binary, whose grants follow its path, runs as
it is. A wrapper running a copy that continues from the
installed release after an upgrade, as an in-place restore does, replaces
itself with a copy of that release and keeps the lock on the copy it started
from, since macOS may go on judging the process by the program it started
with.
