# Unique errors

A launch with 300 failures rarely has 300 problems. FastProof groups failures by their **fingerprint** (the
error type, the normalized message and the top stack frames) and keeps a catalog of every fingerprint a project
has seen. With [FastProof Analysis](analysis.md) on, each of these unique errors gets a life cycle across
branches: when it appeared, on which branches, between which commits, whether it was fixed and whether it came
back. The project's **Errors** page lists them; a click opens one error with its history.

Unique errors need the analysis role (`FASTPROOF_ANALYSIS=on`, see [Configuration](../operations/configuration.md))
and a project that has not turned Analysis off (`analysis.enabled`). The catalog and the `newErrors` gate metric
work without it.

## States

| State | When |
|---|---|
| `new` | It occurred in one launch so far |
| `active` | It occurred again in a later launch |
| `resolved` | It stopped occurring: on **every** branch it occurred on, at least `analysis.resolveAfterLaunches` (default 10) finished launches ran without it; or its linked issue was closed and at least one finished launch ran without it after the close |
| `regressed` | It occurred again after it was resolved; the error counts its regressions |
| `ignored` | A person marked it as noise (for example a flaky DNS in one environment); it is left out of `newErrors` and of notifications and never changes by itself |

Only finished, non-debug launches count. A launch is analysed shortly after it finishes (the analysis role
waits `FASTPROOF_ANALYSIS_SETTLE_SECONDS` for late results). The state is computed from stored facts (where
and when the error occurred and which launches ran since), not from the order in which launches were
analysed, so a long launch that finishes after a newer one gives the same states as launches analysed in
order. Errors linked to a closed issue, and open errors not seen for a day, are checked again every hour.

`resolveAfterLaunches` is a project setting (1 to 100) on **Settings → Analysis** or in `PUT
/api/v1/projects/{key}/settings`:

```json
{"analysis": {"resolveAfterLaunches": 5}}
```

### Branches

Each error remembers on which branches it occurred, with the first and last occurrence and the number of
launches. The newest 50 branches of the project (by their latest launch) and every branch named as a gate
`baseline` are tracked by name; launches of other branches count together as "other branches" (`*` in the
API). An error that occurred on `main` and on a feature branch is resolved only when both had enough launches
without it.

On a launch's **Clusters** tab every cluster says whether its error is new in the project (`isNew`) or only new
on this branch (`newOnBranch`), and its current state (`errorState`).

### Commits

When an error appears (or comes back) FastProof records the commit of that launch and the commit of the
previous finished launch of the same branch, which did not have it: "appeared between `a1b2c3d` and
`d4e5f6a`". With a GitHub or GitLab connection in the project's `vcs` settings the error page links to the
compare view of the two commits. The link is built from the scheme, host and path of `vcs.url` only: user
names, passwords, a query or a fragment in that URL never appear in it.

### Time to resolve

The error page shows the time from the error's first occurrence (or its last regression) to the first launch
without it, once it is resolved.

## Merging and splitting

A change of the message normalizer or of the stack frames can split one real error into two fingerprints.
**Merge into another error** on the error page (or `PATCH` with `aliasOf`) makes a fingerprint an alias of
another: from then on the list, the launch clusters, triage suggestions and `newErrors` count both as one
error, and its occurrences, branches and decisions are shown together. Triaging a launch's cluster
(`launchId` with `fingerprint`) and the results filter `?fingerprint=` of a launch include the results of the
error's aliases. Merging is one level deep: merging into
an alias merges into that alias's error, and errors already merged into the one you merge move along. A
fingerprint cannot be merged into itself or into an error of another project (409 `alias_cycle`). **Split**
(`aliasOf: null`) undoes it; both errors get their states computed again.

## Ignoring

**Ignore** (`state: ignored`) is for errors nobody will fix in the tests: the error stays in the list and keeps
counting occurrences, but it never becomes new, resolved or regressed, `newErrors` leaves it out and
`errorRegressed` is never sent for it. **Activate** (`state: active`) computes its state again from its
occurrences.

## New errors in quality gates

The gate metric `newErrors` counts the unique errors of a launch that are not ignored and did not occur on the
baseline branch in the 30 days before the launch. The baseline branch is the rule's `baseline`, else the
launch's own branch, so a feature branch launch with `baseline: main` fails only for errors that `main` has not
seen. See [Quality gates](quality-gates.md).

## Notifications

Subscribe a channel to `errorRegressed` to hear when errors come back. See
[Notifications](notifications.md#regressed-errors).

## API

All under `/api/v1/projects/{key}`:

| Method, path | Permission | What |
|---|---|---|
| `GET /errors?state=&branch=&q=&sort=&before=&limit=` | `launches:read` | The errors, newest last occurrence first; `sort` is `lastSeen`, `firstSeen`, `occurrences` or `launches`; `q` is a substring of the message (3 characters or more); pages of up to 500 with `next` |
| `GET /errors/{fingerprint}` | `launches:read` | One error: aliases, branches with the launches since its last occurrence, commits, time to resolve, the triage decisions people made for it, its tests and the timeline of the last 30 days |
| `GET /errors/{fingerprint}/timeline?from=&to=` | `launches:read` | Daily occurrences and launches; PostgreSQL keeps 30 days here (`window.partial` beyond), ClickHouse up to 365 |
| `PATCH /errors/{fingerprint}` | `results:triage` | `{"state": "ignored" \| "active"}` or `{"aliasOf": "<fingerprint>" \| null}` |

```bash
curl -s -H "Authorization: Bearer $TOKEN" \
  "https://fastproof.example.com/api/v1/projects/shop/errors?state=regressed&branch=main"

curl -s -X PATCH -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"aliasOf": "3f2a9c1d8e7b6a5f"}' \
  "https://fastproof.example.com/api/v1/projects/shop/errors/a1b2c3d4e5f60718"
```

Patches are written to the audit log as `error.update`.

## Limits

- The list answers within 200 ms for 50 000 errors of a project.
- The sparkline, the test count and the test list read at most 20 000 recent occurrences of a fingerprint,
  and one list page or error page at most 200 000 in all (shared by its errors and their aliases), so the
  sparkline of a very frequent error on a page of 500 shows its most recent occurrences.
- The list and the error page count as heavy requests of the caller (`429` beyond
  `FASTPROOF_HEAVY_QUERIES_PER_CALLER`, 4 by default).
- A launch with more than `FASTPROOF_ANALYSIS_MAX_CLUSTERS` (2000) errors is processed for its largest ones;
  the analysis step is then marked `partial`.
