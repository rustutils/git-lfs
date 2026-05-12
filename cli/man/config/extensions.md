Git LFS extensions wrap each object's bytes through an external program on the clean (commit) and smudge (checkout) paths — useful for repository-wide transforms like compression or encryption that should happen alongside pointerization.

- `lfs.extension.<name>.clean`

  Command run when files are added to the index. Receives the raw bytes on stdin and is expected to emit transformed bytes on stdout.

- `lfs.extension.<name>.smudge`

  Command run when files are written into the working copy. Reverses what `clean` produced.

- `lfs.extension.<name>.priority`

  Sort order across extensions. Lower priorities run first on the clean side, last on the smudge side (so a chain `compress -> encrypt` reverses to `decrypt -> decompress`). Required when more than one extension is configured.

See `git-lfs-ext(1)` for inspecting the resolved chain.
