LFS files reachable from a commit that hasn't reached the remote are never pruned, regardless of age — the local copy is the only one.

'Pushed' is determined by comparing local refs against the remote's refs: any LFS file referenced by a commit reachable from a local ref but not from the corresponding remote ref is treated as unpushed. The pre-push hook uploads LFS objects before the remote branch updates, so this comparison gives an accurate picture.

See DEFAULT REMOTE for which remote anchors the comparison.
