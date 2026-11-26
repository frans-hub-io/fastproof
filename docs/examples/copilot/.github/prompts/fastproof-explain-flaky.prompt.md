---
description: Explain why a FastProof test is flaky
tools: ['fastproof/*', 'search', 'usages']
---

Explain why the test `${input:test}` of project `${input:project}` is flaky. Find it with
`flaky_tests`, then call `flaky_hints` with its `seriesId` (or with its `testCaseId`, which gives up
to ten series). Each hint names a likely cause (for example an environment, a branch, a time of day
or an infrastructure signal) with its evidence and a score; say how strong the evidence is and do
not present a hint as a proven cause. Use `test_history` and `result_logs` of a failed and a passed
attempt to confirm or contradict the top hint, and look at the test and the code under test in this
workspace. Answer with the most likely cause, the evidence, what would confirm it and a suggested
fix. Do not change any triage or mute anything; if a mute or a rule would help, say so and let me
decide.
