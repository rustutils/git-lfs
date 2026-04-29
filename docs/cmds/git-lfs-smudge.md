# git-lfs-smudge

## Name

`git-lfs-smudge` — Run the smudge filter: read a pointer on stdin, write content on stdout

## Synopsis

```
git-lfs-smudge [OPTIONS] [PATH]
```

## Description

Run the smudge filter: read a pointer on stdin, write content on stdout

## Options

### Arguments

- `<PATH>`
    Working-tree path of the file being smudged (currently unused)

### Flags

- `--skip`
    Pass the pointer text through unchanged; equivalent to `GIT_LFS_SKIP_SMUDGE=1`. Wired up by `install --skip-smudge`

