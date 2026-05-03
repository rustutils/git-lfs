You can have `git lfs migrate` convert only files added in commits
reachable from certain references — defined with `--include-ref` —
and ignore files in commits reachable from references defined with
`--exclude-ref`.

For example, given:

        D---E---F
       /         \
      A---B------C    refs/heads/my-feature
       \          \
        \          refs/heads/main
         \
          refs/remotes/origin/main

The commits reachable by each ref:

    refs/heads/main:           C, B, A
    refs/heads/my-feature:     F, E, D, B, A
    refs/remotes/origin/main:  A

The following options would include commits F, E, D, C, and B but
exclude commit A:

    --include-ref=refs/heads/my-feature
    --include-ref=refs/heads/main
    --exclude-ref=refs/remotes/origin/main

The presence of `--everything` indicates that all commits reachable
from all local and remote references should be migrated. Note that
the remote refs themselves are never updated by the migration.
