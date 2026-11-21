# git-lfs-store

Content-addressable on-disk store for Git LFS objects.

Git LFS keeps large files outside Git's object database and commits small
pointer files in their place. This crate manages the local copies of those
files, under `.git/lfs/objects/` in the same layout as upstream Git LFS.

Writes go through a temporary file and are hashed as they're written, so an
interrupted write never leaves a partial object in the store. Downloads collect
in `.git/lfs/incomplete/` until complete, so an interrupted download can resume
where it stopped.

Alternate stores act as fallbacks: an object missing locally is hardlinked or
copied from the first alternate that has it, like Git's
`objects/info/alternates`.

File and directory permissions follow `core.sharedRepository`, as Git's own do.
