# Test scoreboard

Results of upstream's shell tests, as of 2026-11-21.

**748 of 856 tests pass (87%), across 106 suites.**

To refresh, run `cargo xtask test`, which prints these numbers per suite.

## Passing

73 suites pass completely, with 514 tests between them.

## Failing

30 suites have failing tests. 234 of their 342 tests pass.

| Suite | Passing |
| --- | --- |
| `t-batch-error-handling.sh` | 0 of 1 |
| `t-batch-retries-ratelimit.sh` | 5 of 6 |
| `t-batch-storage-corrupt.sh` | 0 of 3 |
| `t-batch-storage-encoding.sh` | 1 of 7 |
| `t-batch-storage-retries-ratelimit.sh` | 5 of 7 |
| `t-batch-storage-upload-tus.sh` | 0 of 2 |
| `t-batch-transfer.sh` | 5 of 9 |
| `t-batch-unknown-oids.sh` | 0 of 1 |
| `t-clone.sh` | 11 of 13 |
| `t-completion.sh` | 0 of 5 |
| `t-config.sh` | 11 of 14 |
| `t-credentials.sh` | 18 of 20 |
| `t-custom-transfers-protect.sh` | 0 of 4 |
| `t-custom-transfers.sh` | 0 of 9 |
| `t-dedup.sh` | 0 of 3 |
| `t-header-redirect.sh` | 5 of 7 |
| `t-lock.sh` | 18 of 19 |
| `t-locks.sh` | 10 of 11 |
| `t-ls-files.sh` | 27 of 31 |
| `t-migrate-export.sh` | 16 of 17 |
| `t-migrate-import.sh` | 44 of 51 |
| `t-multiple-remotes.sh` | 0 of 12 |
| `t-post-checkout.sh` | 2 of 3 |
| `t-progress.sh` | 0 of 4 |
| `t-push.sh` | 26 of 27 |
| `t-ssh.sh` | 0 of 14 |
| `t-standalone-file.sh` | 1 of 9 |
| `t-submodule-lfsconfig.sh` | 3 of 4 |
| `t-unlock.sh` | 26 of 28 |
| `t-usage.sh` | 0 of 1 |

## Skipped

These only run with Git versions too old for the feature they test, and
aren't counted.

- `t-install-custom-hooks-path-unsupported.sh`
- `t-install-worktree-unsupported.sh`
- `t-uninstall-worktree-unsupported.sh`
