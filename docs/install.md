# Installation

`git-lfs` ships three ways: as native packages for the major Linux
distributions, as a Homebrew tap (Linux + macOS), and as a Rust crate
on crates.io. Pick the one that matches your platform.

## Homebrew (Linux and macOS)

```sh
brew tap rustutils/tap
brew install rustutils/tap/git-lfs
```

## APT (Debian and Ubuntu)

Add the signing key, register the repository, and install:

```sh
sudo install -d -m 0755 /etc/apt/keyrings
sudo curl -fsSLo /etc/apt/keyrings/rustutils.asc https://rustutils.gitlab.io/apt/rustutils.asc
echo "deb [signed-by=/etc/apt/keyrings/rustutils.asc] https://rustutils.gitlab.io/apt stable main" \
  | sudo tee /etc/apt/sources.list.d/rustutils.list
sudo apt update
sudo apt install git-lfs-rs
```

The package is named `git-lfs-rs` to avoid colliding with the upstream
Go `git-lfs` package in Debian. The binary is still called `git-lfs`, and
installing it will replace `git-lfs` if you have it installed (but you can
always install `git-lfs` to go back to it).

## RPM (Fedora, RHEL, Rocky, AlmaLinux)

```sh
sudo curl -fsSLo /etc/yum.repos.d/rustutils.repo https://rustutils.gitlab.io/rpm/rustutils.repo
sudo dnf install git-lfs-rs
```

Same `git-lfs-rs` naming convention.

## Cargo (any platform with a Rust toolchain)

```sh
cargo install git-lfs
```

This drops a `git-lfs` binary into `~/.cargo/bin/`. Make sure that
directory is on your `PATH` so `git` can find the executable when it
shells out to invoke filters and hooks.

## After installing

Run once per machine to register the clean, smudge, and process filters
in your global git config:

```sh
git lfs install
```

From there on, `git lfs <command>` works in any repo with a
`.gitattributes` that tracks files via LFS. Per-repository hook
installation happens automatically the first time you run an
LFS-aware command in a new clone, no need to re-run `git lfs install`
per repo.

To enable Git LFS in a repository, you can tell it to track specific
files. For example:

```sh
# track all PDF files in this repository
git lfs track *.pdf
# track all JPEG images in this repository
git lfs track *.jpg
```
