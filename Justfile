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

# Run the full check suite, silent on success. Wired into the
# pre-commit hook (see `install-hooks`). On the first failing step,
# dumps that step's captured output to stderr and exits non-zero —
# you see exactly what broke without re-running anything. If
# `cargo fmt --check` fails, run `cargo fmt && git add -u` to fix
# and re-commit.
pre-commit:
    @echo -e "\033[1;32mChecking\033[0m cargo fmt"
    @out=$(cargo fmt --check 2>&1) || { printf '%s\n' "$out" >&2; exit 1; }
    @echo -e "\033[1;32mChecking\033[0m cargo test"
    @out=$(cargo test --quiet 2>&1) || { printf '%s\n' "$out" >&2; exit 1; }
    @echo -e "\033[1;32mChecking\033[0m cargo doc"
    @out=$(RUSTDOCFLAGS="-Dwarnings" cargo doc --quiet --no-deps 2>&1) || { printf '%s\n' "$out" >&2; exit 1; }
    @echo -e "\033[1;32mChecking\033[0m cargo clippy"
    @out=$(cargo clippy --quiet -- -Dwarnings 2>&1) || { printf '%s\n' "$out" >&2; exit 1; }

# One-time per clone: write `.git/hooks/pre-commit` so every commit
# runs `just pre-commit` first. Idempotent — overwrites any prior
# hook with our wrapper.
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
