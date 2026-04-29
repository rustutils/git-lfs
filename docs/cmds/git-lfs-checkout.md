# git-lfs-checkout

## Name

`git-lfs-checkout` — Replace pointer text in the working tree with actual LFS object content. With no args, materializes every LFS pointer in HEAD's tree. With paths (literal file names or trailing-slash directory prefixes), restricts to matching pointers

## Synopsis

```
git-lfs-checkout [PATHS]...
```

## Description

Replace pointer text in the working tree with actual LFS object content. With no args, materializes every LFS pointer in HEAD's tree. With paths (literal file names or trailing-slash directory prefixes), restricts to matching pointers

## Options

### Arguments

- `<PATHS>`
    Paths to check out. Empty = everything in HEAD's tree

