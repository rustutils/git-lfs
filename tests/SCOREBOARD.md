# Test scoreboard

Results of upstream's shell tests, as of 2026-11-24.

**855 of 856 tests pass (99%), across 106 suites.**

To refresh, run `cargo xtask test`, which prints these numbers per suite.

## Passing

102 suites pass completely, with 855 tests between them.

## Failing

1 suite has failing tests. 0 of its 1 test passes.

| Suite | Passing |
| --- | --- |
| `t-usage.sh` | 0 of 1 |

## Skipped

These only run with Git versions too old for the feature they test, and
aren't counted.

- `t-install-custom-hooks-path-unsupported.sh`
- `t-install-worktree-unsupported.sh`
- `t-uninstall-worktree-unsupported.sh`
