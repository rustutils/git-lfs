`origin` is the default remote consulted for UNPUSHED LFS FILES and VERIFY REMOTE. Even with multiple remotes configured, prune treats this one as canonical — usually it's the main central repo (or your fork of it), and a valid backup of your work.

If `origin` isn't configured, prune treats every reachable LFS file as unpushed and effectively retains everything.

Override the canonical remote with `lfs.pruneremotetocheck`: set it to a different remote name to anchor against that one instead.
