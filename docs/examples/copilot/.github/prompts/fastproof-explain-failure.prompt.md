---
description: Explain why one FastProof result failed
tools: ['fastproof/*', 'search']
---

Explain why the FastProof result `${input:resultId}` of project `${input:project}` failed. Use
`get_result`, `result_logs` (compare attempts when it was retried), `test_history`,
`similar_failures` and `fastmock_interactions`, and look up the test and the code under test in
this workspace. Answer with the most likely cause, the evidence and a suggested fix. Do not change
any triage.
