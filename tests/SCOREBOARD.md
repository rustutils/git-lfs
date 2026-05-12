# Test scoreboard

Per-suite snapshot of the vendored upstream shell tests. Last
refreshed: **2026-05-12**.

**668 / 794 tests passing (~84%) across 104 suites.**

Refresh:

```
cargo xtask test
```

The xtask wrapper runs `make test` under the hood and parses prove's
TAP output into the Full pass / Partial / Empty groups below. Pipe
through `tee` if you want the per-suite breakdown saved.

## Full pass — 68 suites, 455 tests

```
t-alternates.sh                          t-migrate-fixup.sh
t-batch-retries-ratelimit.sh             t-migrate-import-no-rewrite.sh
t-batch-storage-retries-ratelimit.sh     t-no-remote.sh
t-batch-storage-retries.sh               t-object-authenticated.sh
t-batch-transfer-size.sh                 t-path.sh
t-batch-transfer.sh                      t-pointer.sh
t-checkout.sh                            t-post-checkout.sh
t-cherry-pick-commits.sh                 t-post-commit.sh
t-chunked-transfer-encoding.sh           t-post-merge.sh
t-clean.sh                               t-progress-meter.sh
t-clone-deprecated.sh                    t-prune-worktree.sh
t-commit-delete-push.sh                  t-prune.sh
t-config.sh                              t-pull.sh
t-content-type.sh                        t-push-bad-dns.sh
t-credentials-protect.sh                 t-push-failures-local.sh
t-duplicate-oids.sh                      t-push-failures-remote.sh
t-env.sh                                 t-push-file-with-branch-name.sh
t-ext.sh                                 t-reference-clone.sh
t-extra-header.sh                        t-status.sh
t-fetch-include.sh                       t-submodule-lfsconfig.sh
t-fetch-paths.sh                         t-submodule-recurse.sh
t-fetch-recent.sh                        t-submodule.sh
t-fetch.sh                               t-track-attrs.sh
t-filter-branch.sh                       t-track-wildcards.sh
t-filter-process.sh                      t-track.sh
t-fsck.sh                                t-uninstall-worktree.sh
t-happy-path.sh                          t-uninstall.sh
t-install-custom-hooks-path.sh           t-unlock.sh
t-install-worktree.sh                    t-untrack.sh
t-install.sh                             t-unusual-filenames.sh
t-lock.sh                                t-update.sh
t-locks.sh                               t-version.sh
t-malformed-pointers.sh                  t-worktree.sh
t-mergetool.sh                           t-zero-len-file.sh
```

## Partial — 33 suites, 213 / 339 tests

| Suite | Pass / Total |
| --- | --- |
| `t-askpass.sh` | 5 / 6 |
| `t-attributes.sh` | 0 / 4 |
| `t-batch-error-handling.sh` | 0 / 1 |
| `t-batch-storage-encoding.sh` | 0 / 1 |
| `t-batch-storage-upload-tus.sh` | 0 / 2 |
| `t-batch-unknown-oids.sh` | 0 / 1 |
| `t-clone.sh` | 9 / 13 |
| `t-completion.sh` | 0 / 5 |
| `t-credentials-no-prompt.sh` | 1 / 2 |
| `t-credentials.sh` | 5 / 20 |
| `t-custom-transfers.sh` | 0 / 4 |
| `t-dedup.sh` | 0 / 3 |
| `t-expired.sh` | 3 / 6 |
| `t-fetch-refspec.sh` | 2 / 3 |
| `t-logs.sh` | 0 / 1 |
| `t-ls-files.sh` | 14 / 31 |
| `t-merge-driver.sh` | 0 / 6 |
| `t-migrate-export.sh` | 16 / 17 |
| `t-migrate-import.sh` | 44 / 51 |
| `t-migrate-info.sh` | 46 / 50 |
| `t-multiple-remotes.sh` | 0 / 12 |
| `t-pre-push.sh` | 39 / 40 |
| `t-progress.sh` | 0 / 1 |
| `t-push.sh` | 19 / 27 |
| `t-repo-format.sh` | 0 / 1 |
| `t-smudge.sh` | 8 / 9 |
| `t-ssh.sh` | 0 / 2 |
| `t-standalone-file.sh` | 1 / 9 |
| `t-tempfile.sh` | 0 / 1 |
| `t-umask.sh` | 1 / 4 |
| `t-upload-redirect.sh` | 0 / 1 |
| `t-usage.sh` | 0 / 1 |
| `t-verify.sh` | 0 / 4 |

## Skipped — 3 suites

Platform-gated; not counted toward the totals.

- `t-install-custom-hooks-path-unsupported.sh`
- `t-install-worktree-unsupported.sh`
- `t-uninstall-worktree-unsupported.sh`
