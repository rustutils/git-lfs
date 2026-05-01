# List recipes when run without arguments.
default:
    @just --list

# Run the full test harness: Rust unit tests + upstream shell tests.
test: test-unit test-shell

# Run cargo unit tests across the workspace.
test-unit:
    cargo test --release

# Run the upstream shell tests via prove. Requires the Go toolchain
# for the vendored test helpers under tests/cmd/.
test-shell:
    cd tests && make test

# Run a single shell test by filename, e.g.
# just test-one t-clone.sh
test-one TEST:
    cd tests && make ./{{ TEST }}

# Build the release binary at target/release/git-lfs.
build:
    cargo build --release

gen:
    cargo xtask gen-man
    cargo xtask gen-md

# Generate man pages under target/man/. One page per subcommand
# (git-lfs-fetch.1, git-lfs-checkout.1, …) plus a top-level
# git-lfs.1, derived from the clap definition + cli/man/ extras.
man:
    cargo xtask -- gen-man

# Generate markdown reference docs under docs/cmds/. Same shape as
# `man` but emits mdbook-friendly markdown. The output is committed
# to the repo and verified by an xtask snapshot test — re-run this
# whenever you change a clap arg or man-page extra. The rest of
# docs/ (protocol specs, hand-authored prose) is left alone.
docs:
    cargo xtask -- gen-md

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

# Apply formatting and run the full check suite. Used by the
# pre-commit hook (see `install-hooks`). Note: if `cargo fmt`
# rewrites a file you'd already staged, those edits are NOT in the
# commit you're about to make — `git add -u` and re-commit.
pre-commit:
    cargo fmt
    just check

# One-time per clone: write `.git/hooks/pre-commit` so every commit
# runs `just pre-commit` first. Idempotent — overwrites any prior
# hook with our wrapper.
install-hooks:
    #!/usr/bin/env bash
    set -euo pipefail
    hook="$(git rev-parse --git-path hooks)/pre-commit"
    printf '#!/bin/sh\nexec just check\n' > "$hook"
    chmod +x "$hook"
    echo "Installed $hook"

branding:
    typst compile docs/branding/logo.typ docs/branding/logo.svg
    typst compile docs/branding/logo.typ docs/branding/logo.png --ppi 300
    typst compile docs/branding/banner.typ docs/branding/banner-dark.svg  --input theme=dark
    typst compile docs/branding/banner.typ docs/branding/banner-dark.png  --input theme=dark  --ppi 300
    typst compile docs/branding/banner.typ docs/branding/banner-light.svg --input theme=light
    typst compile docs/branding/banner.typ docs/branding/banner-light.png --input theme=light --ppi 300
