# Launch explorer

A launch opens on its **tree**: the tests grouped by suite, behavior, package or any label, with the counts
of every group and the selected test beside it. The **Results** tab lists the same results flat, and the
filters below apply to both.

## Grouping

The **Group by** menu offers presets and a custom grouping:

| Preset | Levels |
|---|---|
| Suites (default) | The suite path the format reports: Allure `parentSuite › suite › subSuite`, Playwright file › describe blocks, JUnit package and class |
| Behaviors | The `epic`, `feature` and `story` labels |
| Packages | The package and class of the full name |
| Owner | The `owner` label |
| Severity | The `severity` label |
| Custom… | Up to five labels of your choice, outermost first |

**Custom…** lists the labels the launch has, most used first, with the number of results that carry each and
its number of values: `team › component`, `parentSuite › suite › subSuite`, `layer` or any custom label.

A result without the label of a level goes to a **(none)** group of that level. Levels after the last label
a result has are left out, so a test with an epic but no feature or story sits directly under its epic. Levels
that no result of the launch has are dropped. When a result has a label more than once, its first value
places it.

The grouping is in the page address (`view=owner`, or `view=custom&by=team,component`), so a link opens the
same tree.

## Filters

The status chips, **Flaky** and the search box sit above the tree. **Filters** opens the rest, each value
with the number of results of the launch (in the chosen environment) that have it:

| Filter | Keeps |
|---|---|
| Retried | Results with more than one attempt |
| New failures | Failed or broken, not muted results whose series did not fail in the previous finished launch of the same branch and environment (every failure when there is none) |
| Muted | Muted results |
| Manual, Automated | Results projected from manual runs, or the others (shown when the launch has manual results) |
| Severity, Owner, Tag | Results with any of the checked values; **No severity** (and so on) keeps those without the label |
| Defect type | Results triaged with any of the checked types; **No defect** keeps the others |
| Attachments | Results with a trace, a video or a screenshot (an image or an image diff), in any attempt, step or fixture |
| Duration | Results that took at least and at most the given seconds |
| Error message | Results whose error message contains the text |

Values of one filter are alternatives; different filters must all match. The filters that are set show as
chips under the toolbar, each with a remove button, and **Clear** removes them all. They are in the page
address too, and the **Results** tab applies the same ones.

## Environments

When the results of a launch ran in more than one environment, the launch page shows an **Environment**
switcher (All, or one of them) and the counts and pass rate of each. Picking one scopes the summary, the tree,
the filter counts and the result list to it.

The environment of a result is the environment key of its series: the launch environment (`--environment`),
and for Playwright reports (JSON, CTRF or JUnit) the project each test ran in, as `launch:project` when both
are set. Results without any key are listed as **No environment**.

Allure 3 assigns each result to an environment by matchers in its configuration and shows one tree per
environment, picked in the report header. FastProof keeps the environment in the series key, so history,
flakiness and transitions stay apart per environment as well, and the switcher gives the same per-environment
view of one launch.

## Large launches

The tree loads groups lazily: a group whose tests do not fit one response (about 5000 tests) reads them page
by page as it scrolls into view, and the rows are virtualized. Filters run on the server, so a filtered tree of
a launch with 100 000 results is no slower than the full one.

Keys: ↑ and ↓ move, → and ← open and close groups, Enter shows a test, Esc closes it. On a phone the test opens
in a full-screen drawer.

## API

| Request | Returns |
|---|---|
| `GET .../launches/{id}/tree?groupBy=` | The tree. `groupBy` is `suite`, `feature`, `package` or up to five label names separated by commas (`label:feature` names the label `feature` rather than the preset). On a label grouping the root lists the `levels` it used; pass them as `groupBy` to `tree/tests`. A `(none)` group has the name `""` |
| `GET .../launches/{id}/tree/tests?groupBy=&path=` | A page of the tests directly in one group |
| `GET .../launches/{id}/results` | The flat list, with the same filters |
| `GET .../launches/{id}/labels?env=` | The label names of the launch with result and value counts (at most 200) |
| `GET .../launches/{id}/facets?env=` | The count of every filter value |
| `GET .../launches/{id}/environments` | The environments of the launch with their status counts |

The tree, its pages and the list take the filters as query parameters: `status`, `q`, `flaky`, `source`,
`retried=true`, `muted=true|false`, `newFailures=true`, `severity`, `owner`, `tag` (comma-separated values,
`(none)` for results without the label), `minDurationMs`, `maxDurationMs`, `defect` (defect type ids or
`none`), `attachment` (`trace`, `video`, `screenshot`), `message`, and `env` (an environment key, or `(none)`).
A filter takes at most 20 values of at most 256 bytes; a label name has at most 64 letters, digits and
`_ - . : @ /`.
