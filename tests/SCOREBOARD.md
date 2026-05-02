# Test scoreboard

Per-suite snapshot of the vendored upstream shell tests. Last
refreshed: **2026-05-02**.

**596 / 794 tests passing (~75%) across 104 suites.**

Refresh:

```
cargo xtask test
```

The xtask wrapper runs `make test` under the hood and parses prove's
TAP output into the Full pass / Partial / Empty groups below. Pipe
through `tee` if you want the per-suite breakdown saved.

## Full pass — 57 suites, 417 tests

```
t-alternates.sh                       t-no-remote.sh
t-batch-transfer-size.sh              t-object-authenticated.sh
t-checkout.sh                         t-path.sh
t-cherry-pick-commits.sh              t-post-checkout.sh
t-chunked-transfer-encoding.sh        t-post-commit.sh
t-clean.sh                            t-post-merge.sh
t-clone-deprecated.sh                 t-pre-push.sh
t-commit-delete-push.sh               t-progress-meter.sh
t-config.sh                           t-pull.sh
t-duplicate-oids.sh                   t-push-bad-dns.sh
t-env.sh                              t-push-failures-local.sh
t-fetch-include.sh                    t-push-failures-remote.sh
t-fetch-paths.sh                      t-push-file-with-branch-name.sh
t-fetch-refspec.sh                    t-reference-clone.sh
t-fetch.sh                            t-status.sh
t-filter-branch.sh                    t-submodule-lfsconfig.sh
t-filter-process.sh                   t-submodule-recurse.sh
t-fsck.sh                             t-submodule.sh
t-happy-path.sh                       t-track-attrs.sh
t-install-custom-hooks-path.sh        t-track-wildcards.sh
t-install-worktree.sh                 t-track.sh
t-install.sh                          t-uninstall-worktree.sh
t-lock.sh                             t-uninstall.sh
t-malformed-pointers.sh               t-unlock.sh
t-mergetool.sh                        t-untrack.sh
t-migrate-export.sh                   t-unusual-filenames.sh
t-migrate-fixup.sh                    t-update.sh
t-migrate-import-no-rewrite.sh        t-version.sh
                                      t-zero-len-file.sh
```

## Partial — 44 suites, 179 / 377 tests

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
| `t-batch-transfer.sh` | 7 / 8 |
| `t-batch-unknown-oids.sh` | 0 / 1 |
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
| `t-locks.sh` | 8 / 9 |
| `t-logs.sh` | 0 / 1 |
| `t-ls-files.sh` | 10 / 31 |
| `t-merge-driver.sh` | 0 / 6 |
| `t-migrate-import.sh` | 44 / 51 |
| `t-migrate-info.sh` | 45 / 50 |
| `t-multiple-remotes.sh` | 0 / 12 |
| `t-pointer.sh` | 20 / 26 |
| `t-progress.sh` | 0 / 1 |
| `t-prune-worktree.sh` | 0 / 2 |
| `t-prune.sh` | 4 / 18 |
| `t-push.sh` | 18 / 27 |
| `t-repo-format.sh` | 0 / 1 |
| `t-smudge.sh` | 7 / 9 |
| `t-ssh.sh` | 0 / 2 |
| `t-standalone-file.sh` | 1 / 9 |
| `t-tempfile.sh` | 0 / 1 |
| `t-umask.sh` | 1 / 4 |
| `t-upload-redirect.sh` | 0 / 1 |
| `t-usage.sh` | 0 / 1 |
| `t-verify.sh` | 0 / 4 |
| `t-worktree.sh` | 0 / 2 |

## Skipped — 3 suites

Platform-gated; not counted toward the totals.

- `t-install-custom-hooks-path-unsupported.sh`
- `t-install-worktree-unsupported.sh`
- `t-uninstall-worktree-unsupported.sh`
