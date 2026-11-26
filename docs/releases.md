# Releases

In Forgejo, open **Actions → release → Run workflow**, select `main`, and enter
`MAJOR.MINOR.PATCH` or leave it blank for the next unused patch version.
Entering the already-tagged version on `main` resumes an interrupted publication
from that immutable tag without creating another version commit.

The workflow waits for the required CI checks on the `main` commit it releases,
and stops unless both passed. It then updates both Cargo versions, creates and
merges a version PR, and publishes its immutable tag and Forgejo release.
[Distribution](design/distribution.md#releases) says which checks it runs and
what the notes hold. Dependent jobs then work from that exact tag:
the bare binary `shoal-vX.Y.Z-<os>-<arch>` is cross-built
for `linux` (static musl) and `macos` on `x86_64` and `arm64`, with
`shoal-vX.Y.Z-macos-<arch>-app.tar.gz` holding only the signed binary as a
signed `Shoal.app`, and attached to the
Forgejo release with a `SHA256SUMS` file, and the same release with the same
files is created on GitHub once the push mirror carries the tag. Both Homebrew
taps then use their host's published checksums to install the binary on Linux
and the `Shoal.app` archive on macOS.
Push mirrors
copy branches and tags only, never releases, which is why GitHub needs its own
token. There is no separate Homebrew or GitHub workflow to run.

## One-time configuration

- `RELEASE_AUTOMATION_TOKEN`: Actions secret for a user/bot with permission to
  push branches/tags, create and merge PRs, and publish releases in Shoal. Its
  identity must be allowed to merge into protected `main`. Grant `write:repository`
  and restrict repository access to `HN05/shoal`. No `read:user` scope is needed;
  release-note reads use the public API without the token.
- `HOMEBREW_TAP_TOKEN`: Actions secret with write access to both Forgejo taps.
- `RELEASE_TOKEN_GITHUB`: fine-grained GitHub token for `HN05/shoal` with
  read and write access to Contents; it creates the GitHub release and assets.
- `MACOS_SIGNING_PEM`: Actions secret holding the unified PEM (certificate and
  private key) of Shoal's self-signed code-signing certificate, which signs the
  macOS binaries and `Shoal.app`. Store the maintainer's existing copy, which
  `rcodesign generate-self-signed-certificate --person-name "Shoal"
  --validity-days 7300` made. A new certificate changes the requirement in
  `.forgejo/scripts/sign-macos.sh`, and
  [every user grants access again](design/distribution.md#macos-signing).
- A `docker` Actions runner with HTTPS access to the forge, registries, and GitHub.

No SSH access is needed. The workflow respects branch protection and will stop
if required approvals or checks prevent merging.
Failed runs retain their release branch or PR for inspection. Existing tags are
never moved. If publication succeeded but a later job failed, rerun the failed
jobs in the same workflow run; they reuse the published tag rather than creating
another release, and a release whose `SHA256SUMS` is attached keeps its assets,
while a partial upload is replaced whole.

Builds run in the CI image described in [ci.md](ci.md), which carries the pinned
Rust toolchain, cargo-nextest, Zig, cargo-zigbuild and rcodesign; rebuild it after
changing its Containerfile.
