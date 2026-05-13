# git-lfs-merge-driver

## Name

`git-lfs-merge-driver` — Merge driver for LFS-tracked files

## Synopsis

```
git-lfs-merge-driver [OPTIONS]
```

## Description

Merge driver for LFS-tracked files

Invoked by Git through a `merge.<name>.driver` configuration entry, typically wired up as:

```text [merge "lfs"] name = LFS merge driver driver = git lfs merge-driver --ancestor %O --current %A --other %B --marker-size %L --output %A ```

For each of `--ancestor`, `--current`, and `--other`, the input file is either a pointer (smudged through to its working-tree content, fetching the object on demand if necessary) or already plain content (used as-is). The three resulting files plus a fresh tempfile for the merged output are substituted into `--program` (default `git merge-file --stdout --marker-size=%L %A %O %B >%D`) and run via `sh -c`. The merged content is then cleaned back into a pointer and written to `--output`. Non-zero exit from the merge program indicates conflicts; that exit code is propagated.

## Options

### Flags

- `--ancestor` `<ANCESTOR>`
    File containing the ancestor (merge-base) version. Pointer or raw content; substituted for `%O` in the program template

- `--current` `<CURRENT>`
    File containing the current (`ours`) version. Pointer or raw content; substituted for `%A` in the program template

- `--other` `<OTHER>`
    File containing the other (`theirs`) version. Pointer or raw content; substituted for `%B` in the program template

- `--output` `<OUTPUT>`
    Path to write the merged pointer to. Typically the same path as `--current` so that Git picks up the result

- `--program` `<PROGRAM>`
    Merge program template. Defaults to `git merge-file --stdout --marker-size=%L %A %O %B >%D`. `%A`, `%O`, `%B`, `%D`, and `%L` are substituted with shell-quoted paths / the marker size; `%%` emits a literal `%`

- `--marker-size` `<MARKER_SIZE>`
    Conflict marker size to substitute for `%L`

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
