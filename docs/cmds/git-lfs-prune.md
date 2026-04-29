# git-lfs-prune

## Name

`git-lfs-prune` — Delete local LFS objects that aren't reachable from HEAD or any unpushed commit. Reclaims disk for repos whose history has moved past their objects

## Synopsis

```
git-lfs-prune [OPTIONS]
```

## Description

Delete local LFS objects that aren't reachable from HEAD or any unpushed commit. Reclaims disk for repos whose history has moved past their objects

## Options

### Flags

- `-d`, `--dry-run`
    Don't delete anything; just report what would go

- `-v`, `--verbose`
    Print each prunable object's OID and size

