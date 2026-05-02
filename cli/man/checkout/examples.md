Checkout all files that are missing or placeholders:

    git lfs checkout

Checkout a specific couple of files:

    git lfs checkout path/to/file1.png path/to/file2.png

Checkout a path with a merge conflict into separate files:

    # Attempt merge with a branch that has a merge conflict
    $ git merge conflicting-branch
    CONFLICT (content): Merge conflict in path/to/conflicting/file.dat

    # Checkout versions of the conflicting file into temp files
    $ git lfs checkout --to ours.dat --ours path/to/conflicting/file.dat
    $ git lfs checkout --to theirs.dat --theirs path/to/conflicting/file.dat

    # Compare conflicting versions in ours.dat and theirs.dat,
    # then resolve conflict (e.g., by choosing one version over
    # the other, or creating a new version)

    # Cleanup and continue with merge
    $ rm ours.dat theirs.dat
    $ git add path/to/conflicting/file.dat
    $ git merge --continue
