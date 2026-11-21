# git-lfs

Large file storage for git, implemented in Rust.

A from-scratch Rust port of [Git LFS][upstream]. The goal is feature parity
with the upstream Go binary at the CLI and protocol level, with better help
output and a clean split into libraries.

## Status

Work in progress, but usable day to day. About 93% of upstream's tests pass
(see the [scoreboard] for details). For production use, upstream Git LFS is
still the safer choice.

The main gaps are custom transfer adapters, tus uploads, `git lfs dedup`,
encrypted client certificate keys, and NTLM and Negotiate authentication.

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

Installing with Cargo doesn't install the man pages. The other methods do.

After installing, run `git lfs install` once per machine to register the Git
LFS filters in your global Git config.

[upstream]: https://github.com/git-lfs/git-lfs
[scoreboard]: https://gitlab.com/rustutils/git-lfs/-/blob/master/tests/SCOREBOARD.md
