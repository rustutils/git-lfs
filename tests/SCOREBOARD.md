# Test scoreboard

Per-suite snapshot of the vendored upstream shell tests. Last
refreshed: **2026-04-30**.

**446 / 794 tests passing (~56%) across 104 suites.**

Refresh:

```
cd tests
rm -rf remote test_count*
make test 2>&1 | tee /tmp/results.log
```

`make test` exits non-zero on any failure, so pipe through `tee` if
you want the per-suite breakdown saved. The Full pass / Partial /
Skipped split below is derived from the `prove` summary at the bottom
of that log — see the log for exact failing test numbers.

## Full pass — 30 suites, 192 tests

```
t-alternates.sh                       t-object-authenticated.sh
t-cherry-pick-commits.sh              t-path.sh
t-chunked-transfer-encoding.sh        t-post-checkout.sh
t-clone-deprecated.sh                 t-post-commit.sh
t-commit-delete-push.sh               t-post-merge.sh
t-config.sh                           t-progress-meter.sh
t-duplicate-oids.sh                   t-push-bad-dns.sh
t-env.sh                              t-push-file-with-branch-name.sh
t-fetch-include.sh                    t-status.sh
t-fetch-paths.sh                      t-submodule-recurse.sh
t-fetch-refspec.sh                    t-submodule.sh
t-fetch.sh                            t-track.sh
t-filter-branch.sh                    t-unlock.sh
t-mergetool.sh                        t-version.sh
t-migrate-export.sh                   t-migrate-import-no-rewrite.sh
```

## Partial — 71 suites, 246 / 602 tests

| Suite | Pass / Total |
| --- | --- |
| `t-askpass.sh` | 1 / 6 |
| `t-attributes.sh` | 0 / 4 |
| `t-batch-error-handling.sh` | 0 / 1 |
| `t-batch-retries-ratelimit.sh` | 0 / 5 |
| `t-batch-storage-encoding.sh` | 0 / 1 |
| `t-batch-storage-retries-ratelimit.sh` | 0 / 5 |
| `t-batch-storage-retries.sh` | 0 / 5 |
| `t-batch-storage-upload-tus.sh` | 0 / 2 |
| `t-batch-transfer-size.sh` | 0 / 2 |
| `t-batch-transfer.sh` | 5 / 8 |
| `t-batch-unknown-oids.sh` | 0 / 1 |
| `t-checkout.sh` | 15 / 18 |
| `t-clean.sh` | 5 / 6 |
| `t-clone.sh` | 9 / 13 |
| `t-completion.sh` | 0 / 5 |
| `t-content-type.sh` | 0 / 3 |
| `t-credentials-no-prompt.sh` | 0 / 2 |
| `t-credentials-protect.sh` | 0 / 3 |
| `t-credentials.sh` | 3 / 20 |
| `t-custom-transfers.sh` | 0 / 4 |
| `t-dedup.sh` | 0 / 3 |
| `t-expired.sh` | 0 / 6 |
| `t-ext.sh` | 0 / 1 |
| `t-extra-header.sh` | 0 / 4 |
| `t-fetch-recent.sh` | 1 / 7 |
| `t-filter-process.sh` | 6 / 8 |
| `t-fsck.sh` | 12 / 16 |
| `t-happy-path.sh` | 4 / 5 |
| `t-install-custom-hooks-path.sh` | 0 / 3 |
| `t-install-worktree.sh` | 0 / 5 |
| `t-install.sh` | 5 / 14 |
| `t-lock.sh` | 15 / 17 |
| `t-locks.sh` | 6 / 9 |
| `t-logs.sh` | 0 / 1 |
| `t-ls-files.sh` | 10 / 31 |
| `t-malformed-pointers.sh` | 1 / 2 |
| `t-merge-driver.sh` | 0 / 6 |
| `t-migrate-fixup.sh` | 11 / 12 |
| `t-migrate-import.sh` | 6 / 51 |
| `t-migrate-info.sh` | 7 / 50 |
| `t-multiple-remotes.sh` | 0 / 12 |
| `t-no-remote.sh` | 1 / 2 |
| `t-pointer.sh` | 20 / 26 |
| `t-pre-push.sh` | 36 / 40 |
| `t-progress.sh` | 0 / 1 |
| `t-prune-worktree.sh` | 0 / 2 |
| `t-prune.sh` | 4 / 18 |
| `t-pull.sh` | 17 / 20 |
| `t-push-failures-local.sh` | 7 / 8 |
| `t-push-failures-remote.sh` | 9 / 10 |
| `t-push.sh` | 18 / 27 |
| `t-reference-clone.sh` | 0 / 2 |
| `t-repo-format.sh` | 0 / 1 |
| `t-smudge.sh` | 4 / 9 |
| `t-ssh.sh` | 0 / 2 |
| `t-standalone-file.sh` | 1 / 9 |
| `t-submodule-lfsconfig.sh` | 1 / 2 |
| `t-tempfile.sh` | 0 / 1 |
| `t-track-attrs.sh` | 1 / 2 |
| `t-track-wildcards.sh` | 1 / 2 |
| `t-umask.sh` | 1 / 4 |
| `t-uninstall-worktree.sh` | 0 / 5 |
| `t-uninstall.sh` | 6 / 10 |
| `t-untrack.sh` | 3 / 7 |
| `t-unusual-filenames.sh` | 0 / 1 |
| `t-update.sh` | 1 / 4 |
| `t-upload-redirect.sh` | 0 / 1 |
| `t-usage.sh` | 0 / 1 |
| `t-verify.sh` | 0 / 4 |
| `t-worktree.sh` | 0 / 2 |
| `t-zero-len-file.sh` | 1 / 2 |

## Skipped — 3 suites

Platform-gated; not counted toward the totals.

- `t-install-custom-hooks-path-unsupported.sh`
- `t-install-worktree-unsupported.sh`
- `t-uninstall-worktree-unsupported.sh`
