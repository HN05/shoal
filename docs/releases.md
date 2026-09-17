# Releases

In Forgejo, open **Actions → release → Run workflow**, select `main`, and enter
`MAJOR.MINOR.PATCH` or leave it blank for the next unused patch version.

The workflow updates both Cargo versions, runs checks and a release build,
creates and merges a version PR, validates the merged commit, and publishes its
immutable tag and Forgejo release. Dependent jobs in the same workflow then update
both Homebrew taps using that exact release tag. The GitHub-facing tap is
push-mirrored to GitHub. There is no separate Homebrew workflow to run.

## One-time configuration

- `RELEASE_AUTOMATION_TOKEN`: Actions secret for a user/bot with permission to
  push branches/tags, create and merge PRs, and publish releases in Shoal. Its
  identity must be allowed to merge into protected `main`. Grant `write:repository`
  and restrict repository access to `HN05/shoal`. No `read:user` scope is needed;
  the workflow uses Git and repository API endpoints only.
- `HOMEBREW_TAP_TOKEN`: Actions secret with write access to both Forgejo taps.
- A `docker` Actions runner with HTTPS access to the forge, registries, and GitHub.

No SSH access is needed. The workflow respects branch protection and will stop
if required approvals or checks prevent merging. It never force-merges.
Failed runs retain their release branch or PR for inspection. Existing tags are
never moved. If publication succeeded but a tap update failed, rerun the failed
jobs in the same workflow run; they reuse the published tag rather than creating
another release.

Builds run in the CI image described in AGENTS.md, which carries the pinned
Rust toolchain and Worktrunk.
The release remains source-built through Homebrew; prebuilt bottles
are not produced by this workflow.
