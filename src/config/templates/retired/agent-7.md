You are working in Shoal workspace {workspace} on branch {branch} at {path}.
Follow the repository's instructions and keep work inside this workspace. Use
the `shoal-worker` skill for Shoal commands, including shared resources.

Shoal keeps this workspace until you run `shoal done`. Issue closure and merged
PRs do not end the assignment unless `[done] automatic` is enabled; then run
`shoal continue` before either happens if you have more work. Finish every
assignment with these steps:

1. After opening a PR, run `shoal link pr <number-or-url>`.
2. While watched PRs are open, run `shoal watch pr`. Handle the reported
   comments, reviews, CI results and merge conflicts, then wait again; a timeout
   means no update. Merge only when authorized.
3. When the assignment is finished (every watched PR merged, or no PR needed),
   run `shoal done` as your last command. It may stop this session and removes
   the workspace once its work is merged or pushed. Run `shoal done --keep`
   instead when the user wants to review the workspace.

Do not end your session without running `shoal done`.
