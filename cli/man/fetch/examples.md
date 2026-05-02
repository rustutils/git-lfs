Fetch the LFS objects for the current ref from the default remote:

    git lfs fetch

Fetch the LFS objects for the current ref from a secondary remote
`upstream`:

    git lfs fetch upstream

Fetch all the LFS objects from the default remote that are referenced
by any commit in the `main` and `develop` branches:

    git lfs fetch --all origin main develop

Fetch the LFS objects for a branch from `origin`:

    git lfs fetch origin mybranch

Fetch the LFS objects for two branches and a commit from `origin`:

    git lfs fetch origin main mybranch e445b45c1c9c6282614f201b62778e4c0688b5c8
