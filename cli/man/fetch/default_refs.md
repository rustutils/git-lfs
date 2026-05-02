If no refs are given as arguments, the currently checked out ref is
used.

Note: upstream's `--recent` mode and the corresponding
`lfs.fetchrecent*` configuration aren't yet supported. The `--recent`
flag is omitted from this implementation; recently changed refs and
commits are not added to the fetch set.
