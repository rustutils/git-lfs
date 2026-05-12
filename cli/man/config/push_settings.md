- `lfs.allowincompletepush`

  When `true`, allow a push to complete even if some LFS objects are missing from the local cache. By default (false), pre-push aborts and the user has to resolve the gap before pushing.

- `lfs.<url>.locksverify` (or unscoped `lfs.locksverify`)

  Controls whether the pre-push hook calls the lock API on the LFS endpoint to refuse pushes over files locked by someone else.

  - `true`: verify locks; halt the push if any are violated or the server is unreachable.
  - `false`: skip the lock check entirely. Set this if you don't use file locking, or your server enforces it server-side.
  - Unset: attempt the call; if it succeeds, persist `true` for next time. If the server returns 501 Not Implemented, persist `false`. If it fails for another reason, warn and continue. (Matches upstream's first-call probe.)
