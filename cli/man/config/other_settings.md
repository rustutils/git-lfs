- `lfs.setlockablereadonly` / `GIT_LFS_SET_LOCKABLE_READONLY`

  Whether files tracked as `lockable` in `.gitattributes` are made read-only in the working copy unless the current user holds the lock. Default `true`. Set either to `0` / `false` / `no` to keep them writeable.

- `lfs.skipdownloaderrors` / `GIT_LFS_SKIP_DOWNLOAD_ERRORS`

  Don't abort the smudge filter when an LFS download fails. The pointer is left in the working tree as-is, and the surrounding `git checkout` (or whatever invoked smudge) reports success. Useful when you need to operate on a repository whose remote is temporarily unavailable, but be aware that scripts checking smudge exit status won't see the failure.

- `GIT_LFS_SKIP_SMUDGE`

  Skip pointer-to-content conversion in `git lfs smudge` and `git lfs filter-process`. Equivalent to running `git lfs install --skip-smudge` (which sets it via `filter.lfs.process`). Any value other than empty / `0` / `false` enables it.

- `GIT_LFS_SKIP_PUSH`

  Make the pre-push hook a no-op. New LFS objects are not uploaded for the duration of the command. Same value semantics as `GIT_LFS_SKIP_SMUDGE`.
