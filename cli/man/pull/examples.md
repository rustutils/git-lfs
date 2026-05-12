Download LFS objects for the current ref from the default remote, then update the working tree:

    git lfs pull

Pull from a specific remote:

    git lfs pull upstream

Pull, but only fetch LFS objects whose paths match a glob (overrides `lfs.fetchinclude` for this invocation):

    git lfs pull -I "textures/**,*.psd"

Pull and skip a path subtree (overrides `lfs.fetchexclude`):

    git lfs pull -X "media/reallybigfiles"
