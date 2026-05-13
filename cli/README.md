# git-lfs

Large file storage for git, implemented in Rust.

A from-scratch Rust port of [upstream Git LFS][upstream]. The
goal is feature parity with the upstream Go binary at the CLI
and wire-protocol level, with a clean library split and better
help output in the binaries.

## Status

Work in progress, but already functional for day-to-day use.
The bulk of upstream's test suite passes; see the [scoreboard]
for the current breakdown. For production workloads, the
upstream Go `git-lfs` is still the answer.

The major gaps are custom transfer adapters, TUS uploads,
pure-SSH transfer (`git-lfs-transfer`), the `dedup` subcommand,
encrypted client certificate keys, and NTLM and Negotiate auth.

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

While installing from Cargo is possible, it means you will not get the
manpages that are available with the other installation methods.

After installing, run `git lfs install` once per machine to
register the clean, smudge, and process filters in your global
git config.

[upstream]: https://github.com/git-lfs/git-lfs
[scoreboard]: https://gitlab.com/rustutils/git-lfs/-/blob/master/tests/SCOREBOARD.md
