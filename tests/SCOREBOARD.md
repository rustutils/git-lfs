# Test scoreboard

Results of upstream's shell tests, as of 2026-11-23.

**851 of 856 tests pass (99%), across 106 suites.**

To refresh, run `cargo xtask test`, which prints these numbers per suite.

## Passing

98 suites pass completely, with 756 tests between them.

## Failing

5 suites have failing tests. 95 of their 100 tests pass.

| Suite | Passing |
| --- | --- |
| `t-batch-storage-encoding.sh` | 6 of 7 |
| `t-migrate-import.sh` | 50 of 51 |
| `t-push.sh` | 26 of 27 |
| `t-ssh.sh` | 13 of 14 |
| `t-usage.sh` | 0 of 1 |

## Skipped

These only run with Git versions too old for the feature they test, and
aren't counted.

- `t-install-custom-hooks-path-unsupported.sh`
- `t-install-worktree-unsupported.sh`
- `t-uninstall-worktree-unsupported.sh`
