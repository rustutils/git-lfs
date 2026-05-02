You can configure Git LFS to only fetch objects to satisfy references
in certain paths of the repo, and/or to exclude certain paths of the
repo, to reduce the time you spend downloading things you do not use.

In your Git configuration or in a `.lfsconfig` file, you may set
either or both of `lfs.fetchinclude` and `lfs.fetchexclude` to
comma-separated lists of paths. If `lfs.fetchinclude` is defined, Git
LFS objects will only be fetched if their path matches one in that
list, and if `lfs.fetchexclude` is defined, Git LFS objects will only
be fetched if their path does not match one in that list. Paths are
matched using wildcard matching as per gitignore(5).

Note that using the command-line options `-I` and `-X` override the
respective configuration settings. Setting either option to an empty
string clears the value.

Examples:

`git config lfs.fetchinclude "textures,images/foo*"`
:   This will only fetch objects referenced in paths in the `textures`
    folder, and files called `foo*` in the `images` folder.

`git config lfs.fetchinclude "*.jpg,*.png,*.tga"`
:   Only fetch JPG/PNG/TGA files, wherever they are in the repository.

`git config lfs.fetchexclude "media/reallybigfiles"`
:   Don't fetch any LFS objects referenced in the folder
    `media/reallybigfiles`, but fetch everything else.

`git config lfs.fetchinclude "media"`<br/>
`git config lfs.fetchexclude "media/excessive"`
:   Only fetch LFS objects in the `media` folder, but exclude those
    in one of its subfolders.
