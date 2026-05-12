List the file types taking up the most space in unpushed commits:

    git lfs migrate info

Check large files and existing LFS objects across every branch (local + remote):

    git lfs migrate info --everything

Report files that should be tracked by Git LFS according to the repository's `.gitattributes` but aren't yet pointers — the candidate set for `git lfs migrate import --fixup`:

    git lfs migrate info --fixup
