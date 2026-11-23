---
description: Defect type ids of the project, by group: `${input:defectTypes}`. If a group is missing here, follow
the agent's rules for finding its id and do not guess.

Triage the newest FastProof launch of a branch
agent: fastproof-triage
argument-hint: project=<key> branch=<branch> defectTypes=<group:id, ...>
---

Defect type ids of the project, by group: `${input:defectTypes}`. If a group is missing here, follow
the agent's rules for finding its id and do not guess.

Triage the newest finished launch of project `${input:project}` on branch `${input:branch}` in
FastProof. Start with the quality gate and the failure clusters, investigate one sample per
cluster, then show me a table of proposed decisions and wait for my confirmation before calling
`triage` or `create_rule_draft`.
