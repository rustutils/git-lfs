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
#   just test-one t-clone.sh
test-one TEST:
    cd tests && make ./{{TEST}}

# Build the release binary at target/release/git-lfs.
build:
    cargo build --release

# Remove cargo build artifacts and shell-test scratch state.
clean:
    cargo clean
    cd tests && make clean
