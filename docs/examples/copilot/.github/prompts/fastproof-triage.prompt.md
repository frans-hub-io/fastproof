---
description: Triage the newest FastProof launch of a branch
agent: fastproof-triage
argument-hint: project=<key> branch=<branch>
---

Triage the newest finished launch of project `${input:project}` on branch `${input:branch}` in
FastProof. Start with the quality gate and the failure clusters, investigate one sample per
cluster, then show me a table of proposed decisions and wait for my confirmation before calling
`triage` or `create_rule_draft`. Take defect types from `list_defect_types`.
