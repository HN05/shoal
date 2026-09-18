# Releases

In Forgejo, open **Actions → release → Run workflow**, select `main`, and enter
`MAJOR.MINOR.PATCH` or leave it blank for the next unused patch version.

The workflow updates both Cargo versions, runs checks and a release build,
creates and merges a version PR, validates the merged commit, and publishes its
immutable tag and Forgejo release. Notes list merged PRs since the nearest earlier
published ancestor release, link referenced issues, and are copied to GitHub.
Dependent jobs then work from that exact tag:
both Homebrew taps are updated, `shoal-vX.Y.Z-<target>.tar.gz` is cross-built
for Linux (static musl) and macOS on x86_64 and aarch64 and attached to the
Forgejo release with a `SHA256SUMS` file, and the same release with the same
files is created on GitHub once the push mirror carries the tag. Push mirrors
copy branches and tags only, never releases, which is why GitHub needs its own
token. There is no separate Homebrew or GitHub workflow to run.

## One-time configuration

- `RELEASE_AUTOMATION_TOKEN`: Actions secret for a user/bot with permission to
  push branches/tags, create and merge PRs, and publish releases in Shoal. Its
  identity must be allowed to merge into protected `main`. Grant `write:repository`
  and restrict repository access to `HN05/shoal`. No `read:user` scope is needed;
  the workflow uses Git and repository API endpoints only.
- `HOMEBREW_TAP_TOKEN`: Actions secret with write access to both Forgejo taps.
- `RELEASE_TOKEN_GITHUB`: fine-grained GitHub token for `HN05/shoal` with
  read and write access to Contents; it creates the GitHub release and assets.
- A `docker` Actions runner with HTTPS access to the forge, registries, and GitHub.

No SSH access is needed. The workflow respects branch protection and will stop
if required approvals or checks prevent merging. It never force-merges.
Failed runs retain their release branch or PR for inspection. Existing tags are
never moved. If publication succeeded but a later job failed, rerun the failed
jobs in the same workflow run; they reuse the published tag rather than creating
another release, and a release whose `SHA256SUMS` is attached keeps its assets,
while a partial upload is replaced whole.

Builds run in the CI image described in AGENTS.md, which carries the pinned
Rust toolchain, Worktrunk, Zig and cargo-zigbuild; rebuild it after changing
its Containerfile. Homebrew still builds from source; the attached binaries are
for direct download.
