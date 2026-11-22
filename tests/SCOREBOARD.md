# Test scoreboard

Results of upstream's shell tests, as of 2026-11-22.

**777 of 856 tests pass (91%), across 106 suites.**

To refresh, run `cargo xtask test`, which prints these numbers per suite.

## Passing

79 suites pass completely, with 594 tests between them.

## Failing

24 suites have failing tests. 183 of their 262 tests pass.

| Suite | Passing |
| --- | --- |
| `t-batch-error-handling.sh` | 0 of 1 |
| `t-batch-storage-corrupt.sh` | 0 of 3 |
| `t-batch-storage-encoding.sh` | 2 of 7 |
| `t-batch-storage-upload-tus.sh` | 0 of 2 |
| `t-batch-unknown-oids.sh` | 0 of 1 |
| `t-clone.sh` | 11 of 13 |
| `t-completion.sh` | 0 of 5 |
| `t-config.sh` | 13 of 14 |
| `t-credentials.sh` | 18 of 20 |
| `t-custom-transfers-protect.sh` | 0 of 4 |
| `t-custom-transfers.sh` | 0 of 9 |
| `t-dedup.sh` | 0 of 3 |
| `t-header-redirect.sh` | 6 of 7 |
| `t-ls-files.sh` | 27 of 31 |
| `t-migrate-export.sh` | 16 of 17 |
| `t-migrate-import.sh` | 44 of 51 |
| `t-multiple-remotes.sh` | 0 of 12 |
| `t-post-checkout.sh` | 2 of 3 |
| `t-progress.sh` | 0 of 4 |
| `t-push.sh` | 26 of 27 |
| `t-ssh.sh` | 13 of 14 |
| `t-standalone-file.sh` | 2 of 9 |
| `t-submodule-lfsconfig.sh` | 3 of 4 |
| `t-usage.sh` | 0 of 1 |

## Skipped

These only run with Git versions too old for the feature they test, and
aren't counted.

- `t-install-custom-hooks-path-unsupported.sh`
- `t-install-worktree-unsupported.sh`
- `t-uninstall-worktree-unsupported.sh`
