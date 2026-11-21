# gotestsum fixtures

Status: **generated** by `tests/fixtures/generators/gotestsum/generate.sh` (real writer, not hand-edited;
only path/host sanitization is applied).

Versions: Go 1.27.3 (linux/arm64), gotestsum v1.13.0.

Regenerate:

```sh
WORK=/path/to/scratch GO=/path/to/go/bin/go tests/fixtures/generators/gotestsum/generate.sh
```

Optional env: `GO_CACHE_DIR` (GOPATH, module and build caches; default inside `WORK`),
`GOTESTSUM` (existing binary; otherwise `go install gotest.tools/gotestsum@v1.13.0`).
Absolute paths are rewritten to `/builds/example/fastproof-samples`, GOROOT to `/usr/local/go`.

| File | Command | Covers |
| --- | --- | --- |
| `junit.xml` | `gotestsum --junitfile junit.xml --rerun-fails=2 --packages="./checkout ./payments" -- -count=1` | pass, `t.Errorf` got/want failure, `t.Skip`, table subtests with one failing subtest, 2-level nested subtests with a skip, flaky test (fails run 1, passes run 2), test failing on all 3 runs; every attempt is a separate `testcase` |
| `junit-norerun.xml` | `gotestsum --junitfile junit-norerun.xml --packages=./... -- -count=1` | same tests in a single run plus `./inventory` with a nil-pointer panic |

The panicking package is left out of the rerun command because gotestsum aborts reruns after a
panic (`ERROR rerun aborted because previous run had a suspected panic and some test may not have run`).
