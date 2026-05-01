# List recipes when run without arguments.
default:
    @just --list

# Run the Rust test suite (unit and integration tests).
test:
    cargo test

# Run the upstream test suite. Pass arg for individual test.
testsuite TEST="test":
    cd tests && make ./{{ TEST }}

# Build the release binary at target/release/git-lfs.
build:
    cargo build --release

# Generate man pages and markdown docs for the git-lfs CLI.
gen:
    cargo xtask gen-man
    cargo xtask gen-md

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
pre-commit:
    @echo -e "\033[1;32mChecking\033[0m cargo fmt"
    @out=$(cargo fmt --check 2>&1) || { printf '%s\n' "$out" >&2; exit 1; }
    @echo -e "\033[1;32mChecking\033[0m cargo test"
    @out=$(cargo test --quiet 2>&1) || { printf '%s\n' "$out" >&2; exit 1; }
    @echo -e "\033[1;32mChecking\033[0m cargo doc"
    @out=$(RUSTDOCFLAGS="-Dwarnings" cargo doc --quiet --no-deps 2>&1) || { printf '%s\n' "$out" >&2; exit 1; }
    @echo -e "\033[1;32mChecking\033[0m cargo clippy"
    @out=$(cargo clippy --quiet -- -Dwarnings 2>&1) || { printf '%s\n' "$out" >&2; exit 1; }

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
