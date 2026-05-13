<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/branding/banner-dark.svg">
  <img alt="git-lfs" src="docs/branding/banner-light.svg">
</picture>

# Git Large File Storage

A from-scratch Rust port of [Git LFS](https://github.com/git-lfs/git-lfs).
The goal is feature parity with the upstream Go binary at the CLI and
wire-protocol level, with a clean library split and a better help output
in the binaries.

## Status

Work in progress. See [`tests/SCOREBOARD.md`](tests/SCOREBOARD.md)
for the per-suite breakdown. The majority of functionality is already
implemented and work end-to-end.

## Why

To be completely honest, the reason I started this was that I didn't
like how the help output of `git-lfs` looks, and I felt I could do
better. Naturally, instead of opening a pull request, I started
reimplementing the whole thing, as one does. And now that I've
started, quitting isn't an option — so Rust is gaining a native
git-lfs.

Jokes aside, implementing git-lfs has been quite a learning exercise.
It's more complex than I would have imagined, but it's also
well-scoped, and the upstream test suite is genuinely good. The aim
is reasonably clean Rust code that can serve as a basis for future
experimentation.

Down the line that could mean plugging into `gitoxide`'s `gix`, or
hosting Git LFS extensions — for example, content-defined chunking to
reduce how much data needs uploading when large files change.

## Installing

**Homebrew** (Linux and macOS):

    brew tap rustutils/tap
    brew install rustutils/tap/git-lfs

**APT** (Debian and Ubuntu):

    sudo install -d -m 0755 /etc/apt/keyrings
    sudo curl -fsSLo /etc/apt/keyrings/rustutils.asc https://rustutils.gitlab.io/apt/rustutils.asc
    echo "deb [signed-by=/etc/apt/keyrings/rustutils.asc] https://rustutils.gitlab.io/apt stable main" \
      | sudo tee /etc/apt/sources.list.d/rustutils.list
    sudo apt update
    sudo apt install git-lfs-rs

**RPM** (Fedora, RHEL, Rocky, AlmaLinux):

    sudo curl -fsSLo /etc/yum.repos.d/rustutils.repo https://rustutils.gitlab.io/rpm/rustutils.repo
    sudo dnf install git-lfs-rs

**Cargo** (any platform with a Rust toolchain):

    cargo install git-lfs

After installing, run `git lfs install` once per machine to register
the clean, smudge, and process filters in your global git config. See
[docs/install.md](docs/install.md) for more details.

## Building and testing

This codebase has two sets of tests. The in-tree Rust tests run
through cargo:

    cargo test

We also ship the upstream git-lfs test suite. Running it requires
`go`, `prove`, and `perl` — see the
[tests README](tests/README.md) for the full setup. Once those are
in place, our xtask runs the suite and prints a per-suite summary:

    cargo xtask test

For the failing suites, the summary includes a count of failing
tests so you can spot near-misses.

Building is done using regular cargo builds:

    cargo build --release

## License

MIT, with attribution to the upstream Git LFS contributors. See
[LICENSE.md](LICENSE.md).
