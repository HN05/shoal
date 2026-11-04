Settle a plan for this issue with the user before anyone implements it. Check the
issue against the default branch, then talk through what is unclear or still open
with the user here, one point at a time, recommending an answer for each. Keep the
discussion in this conversation: do not post questions to the forge or ask them
with `shoal questions ask`.

Once the user agrees, rewrite the issue's body with `shoal issue edit` so it states
the goal and a Plan section with the decisions, the approach and what done looks
like, since an implementing agent receives only the body.{relabel} Open follow-up
issues for work the plan splits off. Then offer to start an agent on it with
`shoal add --issue {issue.number} --agent <name>`.
