# Dashboards

A project has any number of dashboards, up to 50. Each one is a grid of widgets (12 columns) that read the
project's existing aggregates: launch counters, the daily series, flaky statistics and failure fingerprints.
Windows are relative ("the last 30 days"), so a dashboard stays current and can be cloned as it is.

The built-in **Project overview** (`/p/{key}/dashboard`) stays as it was, with its branch, environment and date
filters. **Dashboards** in the project menu lists the ones people made.

## Using a dashboard

- **Edit** switches to edit mode: **Add widget**, move a widget earlier or later (widgets flow left to right in the
  order shown), pick its width (3 to 12 columns) and height, open **Settings**, remove it. **Save** stores the
  layout; if somebody saved in between you are told and nothing is overwritten (`If-Match`).
- **Clone** copies the dashboard into this or another project (you need to be allowed to change dashboards in the
  target). The copy is yours and unlocked; launch ids in widget settings are dropped when the target is another
  project.
- **Lock** makes the dashboard read-only for everybody except its owner and project managers.
- **Share**: the link shown in the header opens the dashboard for anyone who can read the project. There are no
  anonymous links; give people access to the project instead.
- Every chart has a text equivalent for screen readers (a data table), a legend with text labels, and uses the
  status colours of the rest of the UI in both themes.

## Widgets

The catalog follows ReportPortal's widget types where FastProof has the data. (The ReportPortal list below is from
its public documentation as remembered; check it against the current docs when comparing.)

| ReportPortal widget | FastProof widget (`type`) | Data |
| --- | --- | --- |
| Launch statistics chart | `launch-statistics` (bars, lines or areas) | launch counters |
| Overall statistics | `overall-statistics` | launch counters |
| Investigated percentage of launches | `investigated-percentage` | `series_daily` defect groups |
| Launches comparison chart | `launches-comparison` | launch counters |
| Launches duration chart | `launches-duration` | launch start and finish |
| Launches table | `launches-table` | launch counters |
| Failed cases trend chart | `failed-trend` | launch counters |
| Non-passed test cases trend chart | `non-passed-trend` | launch counters |
| Test cases growth trend chart | `test-cases-growth` | `test_cases.first_seen_at` |
| Passing rate per launch | `passing-rate-per-launch` | launch counters |
| Passing rate summary | `passing-rate-summary` | launch counters |
| Flaky test cases table | `flaky-tests` | `series_stats` |
| Most failed test cases table | `most-failing-tests` | `series_daily` |
| Most popular pattern table | `popular-patterns` | `failure_fingerprints` |
| Launch execution and issue statistic | `launch-execution` | one launch: counters and defect groups |
| Component health check, and its table view | `component-health` (`view`: `chart` or `table`) | one launch, grouped by a label |
| Cumulative trend chart | `cumulative-trend` | launch attributes (index kept by a job) and launch counters |
| Unique bugs table | `unique-bugs` | tracker issues and their links ([Issue trackers](issue-trackers.md)) |
| Activity stream | `activity-stream` | the audit log (needs `audit:read`) |
| (FastProof) | `pass-rate-trend`, `durations`, `defect-trend`, `launches-per-day` | `series_daily`, `launch_daily` |

### Cumulative trend

Pick a launch attribute (`attribute`, for example `build` or `release`). The widget groups the launches of the range
by the value of that attribute and adds up their results by status, one bar per value, oldest value first (the
newest `limit` values, at most 50). It reads an index of launch attributes kept by the `launch.attributes.index`
job, which indexes new launches and launches whose attributes were changed within a minute or so, in rounds that give
every project the same share (50 launches each, newest first), up to 500 a run (an upgraded installation fills the index over a few runs). Only launches of the last 730 days are
indexed, at most 20 attributes per launch (keys up to 64 characters, values up to 200). The counters come from the
launches themselves, so triage and late results show without a second copy. The widget reads at most the 5000 most
recent indexed launches that match `branch` and `environment` in the range.

### Unique bugs

Lists the issues the project follows in its tracker ([Issue trackers](issue-trackers.md)): all open ones, and closed
ones that changed within the range, with the state the tracker reported and the number of tests that point to them
(results linked to the issue, plus the series of a linked failure cluster). Open issues come first, then by the number
of tests, among the 500 newest issues (open first) of the project. Issue keys link to the tracker only when the stored address is an `http` or `https` one. It needs no tracker
call: the data is what the tracker sync already stored.

### Activity stream

Shows the latest records of the project in the audit log (action, who, when; never the details of a record). The
audit log has its own permission, so this widget is answered per request and never goes into the data shared between
viewers: people with `audit:read` in the project see the records, everybody else sees a notice in its place. At most
two different activity streams (range and item count) per dashboard.

Not built, and why:

- **Test case search / bug-related tables** that depend on external tracker status beyond what the sync reads:
  use the Issues page.

## Widget settings

All optional; unknown fields are rejected.

| Setting | Meaning | Limit |
| --- | --- | --- |
| `days` | window, days back from now | 1 to 730, default 30 |
| `bucket` | `day` or `week` for trends | default `day` |
| `branch`, `environment` | filter launches | 200 characters |
| `limit` | launches or list items | 1 to 50, default 10; lists of tests hold at most 10 |
| `chart` | `bar`, `line`, `area` (launch statistics) | |
| `view` | `chart` or `table` (component health) | |
| `groupBy` | label name for component health, default `suite` | 64 characters |
| `launchId` | launch for the per-launch widgets, default the latest | |
| `attribute` | launch attribute the cumulative trend groups by | 64 characters |

## Cost and limits

- Every query of a widget runs with a 10 second statement timeout on a small dedicated connection pool (3 connections
  per replica), and one data request has 25 seconds in total; widgets that do not finish answer "took too long"
  while the others still render.
- Saved dashboards share their data between viewers: it is computed once per revision and kept for 60 seconds, so
  open tabs polling every minute cost one computation a minute, not one per viewer. Preview is never cached.
- A dashboard may use at most 4 different (days, bucket, branch, environment) combinations for its trend and test
  list widgets, and at most 3 different per-launch widgets (launch execution and component health); widgets that
  differ only in how they draw share one query.

- A dashboard has at most 3 different cumulative trends (attribute, range, filters) and 2 different activity streams.
- A dashboard has at most 24 widgets; titles 120 characters, names 120, descriptions 2000; a project 50 dashboards.
- Launch widgets read at most 50 launches. Trend widgets with the same filters share one computation per request.
- `launch-execution` and `component-health` read the results of a single launch (grouped in the database, at most
  20 components returned).
- Data requests count as heavy reads (the same per-caller and global caps as the project dashboard) and a statement
  timeout turns one widget into an error entry; the others still render.
- Written data is small: the layout is stored as JSON on the dashboard row. The only background work is the attribute index for the cumulative trend (500 launches a run at most, a minute apart).

## Permissions

| Permission | Allows | Roles |
| --- | --- | --- |
| `launches:read` | list, open, read widget data, clone from | `viewer` and up |
| `audit:read` | see the records of an activity stream widget | `admin` |
| `dashboards:write` | create, change, delete, lock, unlock, clone into, preview unsaved widgets | `editor`, `admin` |
| `settings:write` | change, unlock or delete a dashboard that someone else locked; see who owns dashboards and who changed one last | `admin` |

Every write is an audit entry: `dashboard.create`, `dashboard.update`, `dashboard.delete`, `dashboard.lock`,
`dashboard.unlock`, `dashboard.clone` (in the target project), and `dashboard.clone.out` (in the source project of a
copy into another project).

## API

All under `/api/v1/projects/{key}`:

| Call | Notes |
| --- | --- |
| `GET /dashboards` | `{items}` of summaries |
| `POST /dashboards` | `{name, description, widgets}`; 201; 409 `dashboard_limit` at 50 |
| `GET /dashboards/{id}` | with `ETag: "r<revision>"` |
| `PUT /dashboards/{id}` | needs `If-Match`; 412 with `{currentRevision, changedBy}`; 403 `dashboard_locked` |
| `DELETE /dashboards/{id}` | `If-Match`; 204 |
| `PUT /dashboards/{id}/lock` | `If-Match`; `{locked}` |
| `POST /dashboards/{id}/clone` | `{name?, targetProject?}`; 201 with the copy and its `project` |
| `GET /dashboards/{id}/data` | `{generatedAt, widgets: {id: {status: "ok", data} or {status: "error", message}}}` |
| `POST /dashboard-preview` | `{widgets}`; the data of widgets that are not stored yet; needs `dashboards:write` |

`PUT .../lock` and `DELETE` need `If-Match` like `PUT`. An owner is the caller's identity (managed token id, or
kind and name), not a display name. Owners' names are shown to the owner and to holders of `settings:write`; everyone
else sees "another user" (`owned` tells you whether the caller owns it). Dashboards that existed before this
identity was stored were assigned to `user:<name>`, so one made with a token has no owner any more and is changed
by project managers.

A widget is `{id, type, title, settings, x, y, w, h}`: `id` is 1 to 32 characters of `a-z`, `0-9` and `-`;
`x + w` at most 12; `h` at most 12 rows; `y` at most 1000.
