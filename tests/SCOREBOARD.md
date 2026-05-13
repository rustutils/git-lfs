# Test scoreboard

Per-suite snapshot of the vendored upstream shell tests. Last
refreshed: **2026-05-13**.

**713 / 794 tests passing (~90%) across 104 suites.**

Refresh:

```
cargo xtask test
```

The xtask wrapper runs `make test` under the hood and parses prove's
TAP output into the Full pass / Partial / Empty groups below. Pipe
through `tee` if you want the per-suite breakdown saved.

## Full pass — 77 suites, 584 tests

```
t-alternates.sh                          t-merge-driver.sh
t-attributes.sh                          t-mergetool.sh
t-batch-retries-ratelimit.sh             t-migrate-export.sh
t-batch-storage-retries-ratelimit.sh     t-migrate-fixup.sh
t-batch-storage-retries.sh               t-migrate-import-no-rewrite.sh
t-batch-transfer-size.sh                 t-migrate-info.sh
t-batch-transfer.sh                      t-no-remote.sh
t-checkout.sh                            t-object-authenticated.sh
t-cherry-pick-commits.sh                 t-path.sh
t-chunked-transfer-encoding.sh           t-pointer.sh
t-clean.sh                               t-post-checkout.sh
t-clone-deprecated.sh                    t-post-commit.sh
t-commit-delete-push.sh                  t-post-merge.sh
t-config.sh                              t-pre-push.sh
t-content-type.sh                        t-progress-meter.sh
t-credentials-no-prompt.sh               t-prune-worktree.sh
t-credentials-protect.sh                 t-prune.sh
t-duplicate-oids.sh                      t-pull.sh
t-env.sh                                 t-push-bad-dns.sh
t-expired.sh                             t-push-failures-local.sh
t-ext.sh                                 t-push-failures-remote.sh
t-extra-header.sh                        t-push-file-with-branch-name.sh
t-fetch-include.sh                       t-reference-clone.sh
t-fetch-paths.sh                         t-status.sh
t-fetch-recent.sh                        t-submodule-lfsconfig.sh
t-fetch-refspec.sh                       t-submodule-recurse.sh
t-fetch.sh                               t-submodule.sh
t-filter-branch.sh                       t-track-attrs.sh
t-filter-process.sh                      t-track-wildcards.sh
t-fsck.sh                                t-track.sh
t-happy-path.sh                          t-uninstall-worktree.sh
t-install-custom-hooks-path.sh           t-uninstall.sh
t-install-worktree.sh                    t-unlock.sh
t-install.sh                             t-untrack.sh
t-lock.sh                                t-unusual-filenames.sh
t-locks.sh                               t-update.sh
t-logs.sh                                t-version.sh
t-malformed-pointers.sh                  t-worktree.sh
                                         t-zero-len-file.sh
```

## Partial — 24 suites, 129 / 210 tests

| Suite | Pass / Total |
| --- | --- |
| `t-askpass.sh` | 5 / 6 |
| `t-batch-error-handling.sh` | 0 / 1 |
| `t-batch-storage-encoding.sh` | 0 / 1 |
| `t-batch-storage-upload-tus.sh` | 0 / 2 |
| `t-batch-unknown-oids.sh` | 0 / 1 |
| `t-clone.sh` | 11 / 13 |
| `t-completion.sh` | 0 / 5 |
| `t-credentials.sh` | 6 / 20 |
| `t-custom-transfers.sh` | 0 / 4 |
| `t-dedup.sh` | 0 / 3 |
| `t-ls-files.sh` | 27 / 31 |
| `t-migrate-import.sh` | 44 / 51 |
| `t-multiple-remotes.sh` | 0 / 12 |
| `t-progress.sh` | 0 / 1 |
| `t-push.sh` | 26 / 27 |
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
