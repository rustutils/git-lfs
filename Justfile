# List recipes when run without arguments.
default:
    @just --list

# Run the Rust test suite (unit and integration tests).
test:
    cargo test

# Run the upstream test suite. Pass arg for individual test.
testsuite TEST="test":
    cd tests && make ./{{ TEST }}

# Run the upstream test suite and print a clean per-suite summary.
# Pass suite names (e.g. `pull push`) to limit the run; otherwise runs
# every `t-*.sh`. `--failures` lists per-test failures under each suite.
testsuite-summary *ARGS:
    cargo xtask test {{ ARGS }}

# Build the release binary at target/release/git-lfs.
build:
    cargo build --release

# Generate man pages and markdown docs for the git-lfs CLI.
gen:
    cargo xtask gen-man
    cargo xtask gen-md

# Cross-compiles git-lfs for linux-musl (x86_64, aarch64), darwin
# (x86_64, aarch64), and windows-gnullvm (x86_64, aarch64) using
# cargo-zigbuild, then bundles per-target tarballs (and zips for
# windows). Linux musl targets additionally produce .deb and .rpm
# packages via cargo-deb / cargo-generate-rpm.
#
# Each bundle ships the binary plus the generated man pages, LICENSE.md,
# and README.md, laid out under bin/ and share/ so it untars cleanly
# into ~/.local or /usr/local.
#
# Requires: cargo-zigbuild, zig, zstd, zip, cargo-deb, cargo-generate-rpm.

# Build release artifacts (binaries, .deb, .rpm) into target/dist/.
package:
    #!/usr/bin/env bash
    set -euo pipefail
    rm -rf target/dist
    mkdir -p target/dist

    # Generate man pages once; same files ship with every per-target bundle.
    cargo run -p xtask --release --locked -- gen-man target/man

    build() {
        local target=$1
        rustup target add "$target"
        cargo zigbuild -p git-lfs --release --locked --target "$target"
    }

    bundle_source() {
        local version=$(cargo pkgid --locked -p git-lfs | awk -F'@' '{print $NF}')
        local name="git-lfs-$version"
        local out="target/dist/$name.tar"

        git archive --prefix="$name/" -o "$out" HEAD
        tar -rf "$out" --transform "s|^target/man|$name/man|" target/man/git-lfs*.1
        zstd --force --rm "$out"
    }

    bundle_unix() {
        local target=$1 arch=$2 os=$3
        local name="git-lfs-$arch-$os"
        local stage="target/dist/.stage-$name"
        rm -rf "$stage"
        mkdir -p "$stage/$name/bin" \
                 "$stage/$name/share/man/man1" \
                 "$stage/$name/share/doc/git-lfs"
        cp "target/$target/release/git-lfs" "$stage/$name/bin/"
        cp target/man/git-lfs*.1            "$stage/$name/share/man/man1/"
        cp LICENSE.md README.md             "$stage/$name/share/doc/git-lfs/"
        tar -C "$stage" -cf "target/dist/$name.tar" "$name"
        zstd --rm "target/dist/$name.tar"
        rm -rf "$stage"
    }

    bundle_windows() {
        local target=$1 arch=$2
        local name="git-lfs-$arch-windows"
        local stage="target/dist/.stage-$name"
        rm -rf "$stage"
        mkdir -p "$stage/$name"
        cp "target/$target/release/git-lfs.exe" "$stage/$name/"
        cp target/man/git-lfs*.1                "$stage/$name/"
        cp LICENSE.md README.md                 "$stage/$name/"
        (cd "$stage" && zip -qr "../$name.zip" "$name")
        rm -rf "$stage"
    }

    bundle_source

    # Linux musl: static binary tarball plus .deb and .rpm.
    for arch in x86_64 aarch64; do
        target="$arch-unknown-linux-musl"
        build "$target"
        bundle_unix "$target" "$arch" "linux"
        # --no-strip: rustc already stripped (profile.release.strip=true);
        # cargo-deb's strip uses the host binutils which fails on cross-arch.
        cargo deb -p git-lfs --target "$target" --no-build --no-strip -o target/dist/
        # cargo-generate-rpm's "-p" flag takes a folder, not a package name.
        cargo generate-rpm -p cli --target "$target" -o target/dist/
    done

    # macOS: tarball only (cargo-deb / generate-rpm don't apply).
    for arch in x86_64 aarch64; do
        target="$arch-apple-darwin"
        build "$target"
        bundle_unix "$target" "$arch" "darwin"
    done

    # Windows: zip with .exe. gnullvm targets pair MinGW headers with
    # the LLVM linker, which zigbuild produces natively.
    for arch in x86_64 aarch64; do
        target="$arch-pc-windows-gnullvm"
        build "$target"
        bundle_windows "$target" "$arch"
    done

# Remove cargo build artifacts and shell-test scratch state.
clean:
    cargo clean
    cd tests && make clean

# Run tests and error on warnings
check:
    cargo fmt --check
    cargo test
    RUSTDOCFLAGS="-Dwarnings" cargo doc --no-deps
    cargo clippy -- -Dwarnings

# Pre-commit hook that checks formatting, tests, docs and clippy.
# When invoked from a git pre-commit hook, git sets GIT_DIR /
# GIT_WORK_TREE / GIT_INDEX_FILE to the outer repo. Subprocess `git`
# invocations (including those spawned from our unit tests) then
# silently operate on the outer repo instead of their per-test
# tempdir, breaking 30+ tests. We unset these here so the recipe
# behaves identically whether you run `just pre-commit` from a hook
# or interactively.
pre-commit:
    @echo -e "\033[1;32mChecking\033[0m cargo fmt"
    @out=$(env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE cargo fmt --check 2>&1) || { printf '%s\n' "$out" >&2; exit 1; }
    @echo -e "\033[1;32mChecking\033[0m cargo test"
    @out=$(env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE cargo test --quiet 2>&1) || { printf '%s\n' "$out" >&2; exit 1; }
    @echo -e "\033[1;32mChecking\033[0m cargo doc"
    @out=$(env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE RUSTDOCFLAGS="-Dwarnings" cargo doc --quiet --no-deps 2>&1) || { printf '%s\n' "$out" >&2; exit 1; }
    @echo -e "\033[1;32mChecking\033[0m cargo clippy"
    @out=$(env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE cargo clippy --quiet -- -Dwarnings 2>&1) || { printf '%s\n' "$out" >&2; exit 1; }

# Install the git pre-commit hook.
install-hooks:
    #!/usr/bin/env bash
    set -euo pipefail
    hook="$(git rev-parse --git-path hooks)/pre-commit"
    printf '#!/bin/sh\nexec just pre-commit\n' > "$hook"
    chmod +x "$hook"
    echo "Installed $hook"

branding:
    typst compile docs/branding/logo.typ docs/branding/logo.svg
    typst compile docs/branding/logo.typ docs/branding/logo.png --ppi 300
    typst compile docs/branding/banner.typ docs/branding/banner-dark.svg  --input theme=dark
    typst compile docs/branding/banner.typ docs/branding/banner-dark.png  --input theme=dark  --ppi 300
    typst compile docs/branding/banner.typ docs/branding/banner-light.svg --input theme=light
    typst compile docs/branding/banner.typ docs/branding/banner-light.png --input theme=light --ppi 300
