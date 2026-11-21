# Test scoreboard

Results of upstream's shell tests, as of 2026-11-21.

**737 of 794 tests pass (93%), across 104 suites.**

To refresh, run `cargo xtask test`, which prints these numbers per suite.

## Passing

84 suites pass completely, with 610 tests between them.

## Failing

17 suites have failing tests. 127 of their 184 tests pass.

| Suite | Passing |
| --- | --- |
| `t-batch-error-handling.sh` | 0 of 1 |
| `t-batch-storage-encoding.sh` | 0 of 1 |
| `t-batch-storage-upload-tus.sh` | 0 of 2 |
| `t-batch-unknown-oids.sh` | 0 of 1 |
| `t-clone.sh` | 11 of 13 |
| `t-completion.sh` | 0 of 5 |
| `t-credentials.sh` | 18 of 20 |
| `t-custom-transfers.sh` | 0 of 4 |
| `t-dedup.sh` | 0 of 3 |
| `t-ls-files.sh` | 27 of 31 |
| `t-migrate-import.sh` | 44 of 51 |
| `t-multiple-remotes.sh` | 0 of 12 |
| `t-progress.sh` | 0 of 1 |
| `t-push.sh` | 26 of 27 |
| `t-ssh.sh` | 0 of 2 |
| `t-standalone-file.sh` | 1 of 9 |
| `t-usage.sh` | 0 of 1 |

## Skipped

These only run with Git versions too old for the feature they test, and
aren't counted.

- `t-install-custom-hooks-path-unsupported.sh`
- `t-install-worktree-unsupported.sh`
- `t-uninstall-worktree-unsupported.sh`
