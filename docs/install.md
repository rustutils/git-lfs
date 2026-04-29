# Installation

`git-lfs` is published on [crates.io](https://crates.io/crates/git-lfs).
The simplest way to install it is via `cargo`:

```sh
cargo install git-lfs
```

This drops a `git-lfs` binary into `~/.cargo/bin/`. Make sure that
directory is on your `PATH` so `git` can find the executable when it
shells out to invoke filters and hooks.

Once installed, run `git lfs install` once per machine to register the
clean / smudge / process filters in your global git config. From there
on, `git lfs <command>` works in any repo with a `.gitattributes` that
tracks files via LFS.
