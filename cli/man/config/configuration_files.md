git-lfs reads its configuration from any file `git config -l` returns — that is, the system, global, and per-repository Git config files in their usual precedence order.

A small subset of keys may also be set in a `.lfsconfig` file at the repository root; see LFSCONFIG for the format and the list of keys allowed there. This is useful for settings every clone of the repository should share — most commonly `lfs.url` or an access mode — without forcing each user to configure them manually.

If `.lfsconfig` is missing on disk, the index is checked for it. If that's also missing, `HEAD` is checked. In a bare repository, only `HEAD` is checked.

Settings from Git config files override `.lfsconfig`. This lets you change an LFS-related setting locally (e.g. point `lfs.url` at a staging server) without modifying the repository's tracked configuration.

Most LFS settings live in the `[lfs]` section — keys of the form `lfs.<foo>`. A handful are scoped inside a particular remote's config (`remote.<name>.lfsurl` and similar) and override the global `lfs.*` equivalents for that remote.

URL-specific overrides are written as `lfs.<url>.<key>`, where `<url>` is the LFS endpoint the setting should apply to. Longest-prefix match wins, so `lfs.https://lfs.example.com/.locksverify` overrides `lfs.locksverify` only for that endpoint.
