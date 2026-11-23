# Test scoreboard

Results of upstream's shell tests, as of 2026-11-23.

**853 of 856 tests pass (99%), across 106 suites.**

To refresh, run `cargo xtask test`, which prints these numbers per suite.

## Passing

100 suites pass completely, with 821 tests between them.

## Failing

3 suites have failing tests. 32 of their 35 tests pass.

| Suite | Passing |
| --- | --- |
| `t-batch-storage-encoding.sh` | 6 of 7 |
| `t-push.sh` | 26 of 27 |
| `t-usage.sh` | 0 of 1 |

## Skipped

These only run with Git versions too old for the feature they test, and
aren't counted.

- `t-install-custom-hooks-path-unsupported.sh`
- `t-install-worktree-unsupported.sh`
- `t-uninstall-worktree-unsupported.sh`
