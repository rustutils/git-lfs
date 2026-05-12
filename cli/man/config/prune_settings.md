- `lfs.pruneoffsetdays`

  Extra days added to the `lfs.fetchrecent*days` windows when deciding what prune can delete. A ref or commit has to be at least this many days older than the oldest one `fetch --recent` would download for prune to treat it as old enough to delete. Default 3. Only takes effect when the underlying fetch-recent setting is non-zero.

- `lfs.pruneremotetocheck`

  Remote to consult for UNPUSHED LFS FILES detection and `--verify-remote`. Default `origin`. See git-lfs-prune(1) for the full retention rules.

- `lfs.pruneverifyremotealways`

  When `true`, always run prune as if `--verify-remote` was passed. The pre-delete remote-presence check applies on every invocation. Use `--no-verify-remote` to opt out for a single run.

- `lfs.pruneverifyunreachablealways`

  When `true`, always run prune as if `--verify-unreachable` was passed — also verify objects not reachable from any commit. Only meaningful when remote verification is on. Use `--no-verify-unreachable` to opt out for a single run.
