# git-lfs-track

## Name

`git-lfs-track` — View or add Git LFS paths to Git attributes

## Synopsis

```
git-lfs-track [OPTIONS] [PATTERNS]...
```

## Description

View or add Git LFS paths to Git attributes

Start tracking the given pattern(s) through Git LFS. The argument is written to `.gitattributes`. If no paths are provided, list the currently-tracked paths.

Per [gitattributes(5)](https://git-scm.com/docs/gitattributes), patterns use the [gitignore(5)](https://git-scm.com/docs/gitignore) pattern rules to match paths. This means that patterns containing asterisk (`*`), question mark (`?`), and the bracket characters (`[` and `]`) are treated specially; to disable this behavior and treat them literally instead, use `--filename` or escape the character with a backslash.

## Options

### Arguments

- `<PATTERNS>`
    File patterns to track (e.g. `*.jpg`, `data/*.bin`)

### Flags

- `-v`, `--verbose`
    Log files which `git lfs track` will touch. Disabled by default

- `-d`, `--dry-run`
    Log all actions that would normally take place (adding entries to `.gitattributes`, touching files on disk, etc.) without performing any mutative operations.

    Implicitly mocks the behavior of `--verbose`, logging in greater detail what it is doing. Disabled by default.

- `-j`, `--json`
    Write the currently tracked patterns as JSON to standard output.

    Intended for interoperation with external tools. Cannot be combined with any pattern arguments. If `--no-excluded` is also provided, that option will have no effect.

- `--filename`
    Treat the arguments as literal filenames, not as patterns.

    Any special glob characters in the filename will be escaped when writing the `.gitattributes` file.

- `-l`, `--lockable`
    Make the paths "lockable" — they should be locked to edit them, and will be made read-only in the working copy when not locked

- `--not-lockable`
    Remove the lockable flag from the paths so they are no longer read-only unless locked

- `--no-excluded`
    Don't list patterns that are excluded in the output; only list patterns that are tracked

- `--no-modify-attrs`
    Make matched entries stat-dirty so that Git can re-index files you wish to convert to LFS.

    Does not modify any `.gitattributes` file.

## Examples

List the patterns that Git LFS is currently tracking:

    git lfs track

Configure Git LFS to track GIF files:

    git lfs track "*.gif"

Configure Git LFS to track PSD files and make them read-only unless
locked:

    git lfs track --lockable "*.psd"

Configure Git LFS to track the file named `project [1].psd`:

    git lfs track --filename "project [1].psd"

## See also

[git-lfs-untrack(1)](./git-lfs-untrack.md), [git-lfs-install(1)](./git-lfs-install.md), [gitattributes(5)](https://git-scm.com/docs/gitattributes), [gitignore(5)](https://git-scm.com/docs/gitignore).

## Reporting bugs

This command is from the Rust implementation of git-lfs, not the original
Go implementation. Please report bugs to our [issue tracker](https://gitlab.com/rustutils/git-lfs/issues).
