# Test scoreboard

Per-suite snapshot of the vendored upstream shell tests. Last
refreshed: **2026-11-21**.

**737 / 794 tests passing (~93%) across 104 suites.**

Refresh:

```
cargo xtask test
```

The xtask wrapper runs `make test` under the hood and parses prove's
TAP output into the Full pass / Partial / Empty groups below. Pipe
through `tee` if you want the per-suite breakdown saved.

## Full pass — 84 suites, 610 tests

```
t-alternates.sh                          t-migrate-fixup.sh
t-askpass.sh                             t-migrate-import-no-rewrite.sh
t-attributes.sh                          t-migrate-info.sh
t-batch-retries-ratelimit.sh             t-no-remote.sh
t-batch-storage-retries-ratelimit.sh     t-object-authenticated.sh
t-batch-storage-retries.sh               t-path.sh
t-batch-transfer-size.sh                 t-pointer.sh
t-batch-transfer.sh                      t-post-checkout.sh
t-checkout.sh                            t-post-commit.sh
t-cherry-pick-commits.sh                 t-post-merge.sh
t-chunked-transfer-encoding.sh           t-pre-push.sh
t-clean.sh                               t-progress-meter.sh
t-clone-deprecated.sh                    t-prune-worktree.sh
t-commit-delete-push.sh                  t-prune.sh
t-config.sh                              t-pull.sh
t-content-type.sh                        t-push-bad-dns.sh
t-credentials-no-prompt.sh               t-push-failures-local.sh
t-credentials-protect.sh                 t-push-failures-remote.sh
t-duplicate-oids.sh                      t-push-file-with-branch-name.sh
t-env.sh                                 t-reference-clone.sh
t-expired.sh                             t-repo-format.sh
t-ext.sh                                 t-smudge.sh
t-extra-header.sh                        t-status.sh
t-fetch-include.sh                       t-submodule-lfsconfig.sh
t-fetch-paths.sh                         t-submodule-recurse.sh
t-fetch-recent.sh                        t-submodule.sh
t-fetch-refspec.sh                       t-tempfile.sh
t-fetch.sh                               t-track-attrs.sh
t-filter-branch.sh                       t-track-wildcards.sh
t-filter-process.sh                      t-track.sh
t-fsck.sh                                t-umask.sh
t-happy-path.sh                          t-uninstall-worktree.sh
t-install-custom-hooks-path.sh           t-uninstall.sh
t-install-worktree.sh                    t-unlock.sh
t-install.sh                             t-untrack.sh
t-lock.sh                                t-unusual-filenames.sh
t-locks.sh                               t-update.sh
t-logs.sh                                t-upload-redirect.sh
t-malformed-pointers.sh                  t-verify.sh
t-merge-driver.sh                        t-version.sh
t-mergetool.sh                           t-worktree.sh
t-migrate-export.sh                      t-zero-len-file.sh
```

## Partial — 17 suites, 127 / 184 tests

| Suite | Pass / Total |
| --- | --- |
| `t-batch-error-handling.sh` | 0 / 1 |
| `t-batch-storage-encoding.sh` | 0 / 1 |
| `t-batch-storage-upload-tus.sh` | 0 / 2 |
| `t-batch-unknown-oids.sh` | 0 / 1 |
| `t-clone.sh` | 11 / 13 |
| `t-completion.sh` | 0 / 5 |
| `t-credentials.sh` | 18 / 20 |
| `t-custom-transfers.sh` | 0 / 4 |
| `t-dedup.sh` | 0 / 3 |
| `t-ls-files.sh` | 27 / 31 |
| `t-migrate-import.sh` | 44 / 51 |
| `t-multiple-remotes.sh` | 0 / 12 |
| `t-progress.sh` | 0 / 1 |
| `t-push.sh` | 26 / 27 |
| `t-ssh.sh` | 0 / 2 |
| `t-standalone-file.sh` | 1 / 9 |
| `t-usage.sh` | 0 / 1 |

## Skipped — 3 suites

Platform-gated; not counted toward the totals.

- `t-install-custom-hooks-path-unsupported.sh`
- `t-install-worktree-unsupported.sh`
- `t-uninstall-worktree-unsupported.sh`
