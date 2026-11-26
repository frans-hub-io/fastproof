---
description: Triage the newest FastProof launch of a branch
agent: fastproof-triage
argument-hint: project=<key> branch=<branch>
---

Triage the newest finished launch of project `${input:project}` on branch `${input:branch}` in
FastProof. Start with the quality gate, then call `launch_suggestions` and use its bands; for
clusters without a `same` or `high` suggestion investigate one sample per cluster (logs, mock
interactions, history). Show me a table of proposed decisions and wait for my confirmation before
calling `accept_suggestion`, `reject_suggestion`, `triage` or `create_rule_draft`. Take defect
types from `list_defect_types`. Tell me the batch id of every write so I can ask for
`undo_triage`.
