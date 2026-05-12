`--verify-remote` (`-c`) asks the remote whether every prunable LFS file has a server-side copy before deleting it locally. The UNPUSHED LFS FILES check above is usually enough, but `--verify-remote` adds belt-and-braces for cases where you want to be sure (at the cost of extra batch calls to the server).

Enable as the default by setting `lfs.pruneverifyremotealways=true`.

`--verify-unreachable` extends the verification pass to LFS objects that aren't referenced by any commit (orphans — added to the index but never committed, or referenced only by orphaned commits). Without this flag, orphans pass through `--verify-remote` silently and are deleted. Enable as the default with `lfs.pruneverifyunreachablealways=true`.

By default, `--verify-remote` halts the entire prune if any object can't be verified. Pass `--when-unverified=continue` to instead drop the unverifiable objects from the delete set and proceed with the rest.

See DEFAULT REMOTE for which remote is queried.
