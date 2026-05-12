Convert all zip Git LFS pointers on `main` back to regular Git blobs:

    git lfs migrate export --include-ref=main --include="*.zip"

Pointers whose objects aren't in the local store are downloaded from the `--remote` (defaults to `origin`); pointers that can't be downloaded are left as-is.

After exporting, the rewritten branches need to be force-pushed — this rewrites history on the remote.
