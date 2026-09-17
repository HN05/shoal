# Releases

In Forgejo, open **Actions → Release → Run workflow**, select `main`, and enter
`MAJOR.MINOR.PATCH` or leave it blank for the next unused patch version.

The workflow updates both Cargo versions, runs checks and a release build,
creates and merges a version PR, validates the merged commit, and publishes its
immutable tag and Forgejo release. Publication triggers automatic updates to
both Homebrew taps. The GitHub-facing tap is push-mirrored to GitHub.

## One-time configuration

- `RELEASE_AUTOMATION_TOKEN`: Actions secret for a user/bot with permission to
  push branches/tags, create and merge PRs, and publish releases in Shoal. Its
  identity must be allowed to merge into protected `main`. A personal token
  ensures publication triggers the Homebrew workflow. Grant `write:repository`
  and restrict repository access to `HN05/shoal`. No `read:user` scope is needed;
  the workflow uses Git and repository API endpoints only.
- `HOMEBREW_TAP_TOKEN`: Actions secret with write access to both Forgejo taps.
- A `docker` Actions runner with HTTPS access to the forge, registries, and GitHub.

No SSH access is needed. The workflow respects branch protection and will stop
if required approvals or checks prevent merging. It never force-merges.
Failed runs retain their release branch or PR for inspection. Existing tags are
never moved. If a release has already been published, retry its Homebrew update
using **Update Homebrew release** with that tag.

Builds use the public Rust image and install a pinned Worktrunk version.
The release remains source-built through Homebrew; prebuilt bottles
are not produced by this workflow.
