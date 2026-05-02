# git-lfs-clean

## Name

`git-lfs-clean` — Git clean filter that converts large files to pointers

## Synopsis

```
git-lfs-clean [PATH]
```

## Description

Git clean filter that converts large files to pointers

Read the contents of a large file from standard input, and write a Git LFS pointer file for that file to standard output.

Clean is typically run by Git’s clean filter, configured by the repository’s Git attributes.

Clean is not part of the user-facing Git plumbing commands. To preview the pointer of a large file as it would be generated, see the [git-lfs-pointer(1)](./git-lfs-pointer.md) command.

## Options

### Arguments

- `<PATH>`
    Working-tree path of the file being cleaned.

    Substituted for `%f` in any configured `lfs.extension.<name>.clean` command.

