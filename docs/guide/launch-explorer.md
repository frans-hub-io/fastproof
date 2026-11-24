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

A result without the label of a level goes to a **(none)** group of that level; an empty value counts as no
value. Levels after the last label a result has are left out, so a test with an epic but no feature or story
sits directly under its epic. Levels that no result of the launch has are dropped.

When a result has a label more than once with different values (two `owner` labels, two `feature` labels), it
appears in the group of each value, as in Allure 3; with several such levels, in each combination. A repeated
identical value counts once. Every group above counts the result once, so the counts of a parent can be less
than the sum of its children. A result is placed by at most 20 values of one label and in at most 64 groups. A
filter by a label value matches any value of the result, so a result with more than 20 values of a label can
match a filter by its 21st value and still show only in the groups of its first 20. A tree holds at most
200 000 groups (and reads at most 200 000 distinct placements); beyond that the root says `partial` and the
counts cover the groups listed.

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
| Error message | Results whose error message contains all the words of the text (whole words, not parts of words; the first 64 Ki characters of a message are searched) |

Values of one filter are alternatives; different filters must all match. The filters that are set show as
chips under the toolbar, each with a remove button, and **Clear** removes them all. They are in the page
address too, and the **Results** tab applies the same ones.

## Environments

When the results of a launch ran in more than one environment, the launch page shows an **Environment**
switcher (All, or one of them) and the counts and pass rate of each. Picking one scopes the whole launch page
to it: the summary, the tree, the filter counts, the result list, the **Failures** tab (clusters and their
results) and the **Logs** tab. A log line belongs to an environment once the result of its test is stored, so
while a launch runs, the Logs tab of one environment shows the stored lines of finished tests and reads them
again every 10 seconds instead of streaming; **All** streams every line live.

The environment of a result is the environment key of its series: the launch environment (`--environment`),
and for Playwright reports (JSON, CTRF or JUnit) the project each test ran in, as `launch:project` when both
are set. Results without any key are listed as **No environment**.

Allure 3 assigns each result to an environment by matchers in its configuration and shows one tree per
environment, picked in the report header. FastProof keeps the environment in the series key, so history,
flakiness and transitions stay apart per environment as well, and the switcher gives the same per-environment
view of one launch.

## Large launches

The tree loads groups lazily: a group whose tests do not fit one response (about 5000 tests) reads them page
by page as it scrolls into view, and the rows are virtualized. The server counts the groups in the database and
reads only the tests of the groups it sends inline. Filters run on the server, so a filtered tree of a launch
with 100 000 results is no slower than the full one.

For a launch that has finished, the server keeps the counts of the unfiltered tree (per grouping and
environment), the label names, the filter counts and the environments, so opening the launch again is fast.
When a launch of 2000 results or more finishes, the server builds its unfiltered Suites tree in the
background within a minute, so the first person to open it does not wait for the count either. The tests of
the Suites groups the tree shows are found through an index, without reading the whole launch.
Any change to the launch's results (late results, triage, mutes, deletion) makes them count again. The label
names and the filter counts read at most 200 000 results; `partial` is true when a launch has more.

After an upgrade, a background job fills the label index and the stored suite path of results stored by
earlier versions, and the
attachment kinds of results stored before the explorer had attachment filters (see
[Upgrading](../operations/upgrading.md#020-dev1-faster-launch-explorer)). Until it reaches a launch, its tree
is built the slower way and its attachment filter may miss results.

Keys: ↑ and ↓ move, → and ← open and close groups, Enter shows a test, Esc closes it. On a phone the test opens
in a full-screen drawer.

## API

| Request | Returns |
|---|---|
| `GET .../launches/{id}/tree?groupBy=` | The tree. `groupBy` is `suite`, `feature`, `package` or up to five label names separated by commas (`label:feature` names the label `feature` rather than the preset). On a label grouping the root lists the `levels` it used; pass them as `groupBy` to `tree/tests`. A `(none)` group has the name `""` |
| `GET .../launches/{id}/tree/tests?groupBy=&path=` | A page of the tests directly in one group |
| `GET .../launches/{id}/results` | The flat list, with the same filters |
| `GET .../launches/{id}/labels?env=` | The label names of the launch with result and value counts (at most 200), and `partial` |
| `GET .../launches/{id}/facets?env=` | The count of every filter value, and `partial` |
| `GET .../launches/{id}/clusters?env=` | The failure clusters of the Failures tab, in one environment |
| `GET .../launches/{id}/logs?env=` | Stored log lines of the results in one environment |
| `GET .../launches/{id}/environments` | The environments of the launch with their status counts |

The tree, its pages and the list take the filters as query parameters: `status`, `q`, `flaky`, `source`,
`retried=true`, `muted=true|false`, `newFailures=true`, `severity`, `owner`, `tag` (comma-separated values,
`%00` for results without the label), `minDurationMs`, `maxDurationMs`, `defect` (defect type ids or
`none`), `attachment` (`trace`, `video`, `screenshot`), `message`, and `env` (an environment key, or `%00` for results without one; the same `%2C` and `%25` escapes apply).
In a list, a comma inside a value is written `%2C` and a percent sign `%25` (on top of the URL
encoding, so `a,b` is `a%252Cb` in a raw query string); a value that reads `(none)` is an ordinary value. Plain
comma-separated lists work as before. `message` matches whole words (all of them, any order), not parts of words.
A filter takes at most 20 values of at most 256 bytes; a label name has at most 64 letters, digits and
`_ - . : @ /`.
