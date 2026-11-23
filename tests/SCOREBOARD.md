# Test scoreboard

Results of upstream's shell tests, as of 2026-11-23.

**850 of 856 tests pass (99%), across 106 suites.**

To refresh, run `cargo xtask test`, which prints these numbers per suite.

## Passing

97 suites pass completely, with 753 tests between them.

## Failing

6 suites have failing tests. 97 of their 103 tests pass.

| Suite | Passing |
| --- | --- |
| `t-batch-storage-encoding.sh` | 6 of 7 |
| `t-migrate-import.sh` | 50 of 51 |
| `t-post-checkout.sh` | 2 of 3 |
| `t-push.sh` | 26 of 27 |
| `t-ssh.sh` | 13 of 14 |
| `t-usage.sh` | 0 of 1 |

## Skipped

These only run with Git versions too old for the feature they test, and
aren't counted.

- `t-install-custom-hooks-path-unsupported.sh`
- `t-install-worktree-unsupported.sh`
- `t-uninstall-worktree-unsupported.sh`
