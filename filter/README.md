# git-lfs-filter

Clean and smudge filters and the filter-process protocol for Git LFS.

Git runs filters when files move between the working tree and the repository.
The clean filter runs when a file is added: it stores the file in the LFS
store, and gives Git a small pointer file to commit instead. The smudge filter
runs on checkout: it finds the file content for a pointer, downloading it if
needed, and writes it to the working tree.

This crate implements both filters, and Git's [long-running filter
protocol][filter-process], which handles many files in one process. The entry
points `clean`, `smudge`, and `filter_process` are the bodies of `git lfs
clean`, `git lfs smudge`, and `git lfs filter-process`.

LFS extensions are external programs that transform file content on the way in
and reverse it on the way out, for things like encryption. They're configured
with `lfs.extension.<name>.*` settings. Clean runs them in priority order and
records each step in the pointer, and smudge runs them in reverse.

[filter-process]:
https://git-scm.com/docs/gitattributes#_long_running_filter_process
