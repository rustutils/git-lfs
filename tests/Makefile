SHELL = bash
RM ?= rm -f
PROVE ?= prove
PROVE_EXTRA_ARGS =
DEFAULT_TEST_TARGET ?= test

GO ?= go
CARGO ?= cargo

# Local Go toolchain only — don't auto-download.
export GOTOOLCHAIN = local

ifeq ($(OS),Windows_NT)
X ?= .exe
else
X ?=
endif

# Helper binaries the upstream shell tests rely on, built from the Go
# sources vendored in `cmd/`. The upstream helpers that depend on upstream's
# internal Go packages are ported to Rust in `cmd/src/bin/` instead, and
# `lfstest-gitserver` is ported in `server/`.
TEST_CMDS =
TEST_CMDS += ../bin/git-credential-lfsnoop$X
TEST_CMDS += ../bin/git-credential-lfstest$X
TEST_CMDS += ../bin/lfs-askpass$X
TEST_CMDS += ../bin/lfs-ssh-echo$X
TEST_CMDS += ../bin/lfs-ssh-proxy-test$X
TEST_CMDS += ../bin/lfstest-badlocalfile$X
TEST_CMDS += ../bin/lfstest-caseinverterextension$X
TEST_CMDS += ../bin/lfstest-count-tests$X
TEST_CMDS += ../bin/lfstest-exploitcheck$X
TEST_CMDS += ../bin/lfstest-genrandom$X
TEST_CMDS += ../bin/lfstest-getlimit$X
TEST_CMDS += ../bin/lfstest-getnumcpu$X
TEST_CMDS += ../bin/lfstest-nanomtime$X
TEST_CMDS += ../bin/lfstest-realpath$X

GIT_LFS_BIN = ../bin/git-lfs$X
RUST_TEST_CMDS =
RUST_TEST_CMDS += ../bin/lfstest-customadapter$X
RUST_TEST_CMDS += ../bin/lfstest-gitserver$X
RUST_TEST_CMDS += ../bin/lfstest-standalonecustomadapter$X
RUST_TEST_CMDS += ../bin/lfstest-testutils$X

TEST_SRCS = $(wildcard t-*.sh)

all : $(DEFAULT_TEST_TARGET)

test-commands : $(TEST_CMDS) $(GIT_LFS_BIN) $(RUST_TEST_CMDS)

test : test-commands
	$(RM) -r remote test_count{,.lock}
	@. ./testenv.sh && setup && cd tests && \
		RM_GIT_LFS_TEST_DIR=no $(PROVE) $(PROVE_EXTRA_ARGS) t-*.sh && \
		shutdown

.PHONY : $(TEST_SRCS)
$(TEST_SRCS) : $(TEST_CMDS) $(GIT_LFS_BIN) $(RUST_TEST_CMDS)
	$(RM) -r remote test_count{,.lock}
	$(PROVE) -v $(PROVE_EXTRA_ARGS) $@

.PHONY : clean
clean :
	$(RM) -r remote
	$(RM) $(TEST_CMDS) $(GIT_LFS_BIN) $(RUST_TEST_CMDS)

# Build each Go helper individually. cd into cmd/ so its go.mod
# takes effect (it's a self-contained module separate from the
# Rust workspace).
../bin/%$X : cmd/%.go
	@mkdir -p ../bin
	cd cmd && $(GO) build -o ../$@ ./$*.go

# The Rust binaries that the tests drive. Built in release mode for
# realistic runtime behavior. Phony — `cargo build` is the cache.
#
# We symlink rather than copy so macOS on Apple Silicon doesn't
# SIGKILL the copy: a `cp` of an ad-hoc-signed Mach-O loses kernel-
# level provenance metadata and the loader refuses to run it. A
# symlink resolves to the original file, which the kernel trusts.
.PHONY : $(GIT_LFS_BIN) $(RUST_TEST_CMDS)
$(GIT_LFS_BIN) :
	cd .. && $(CARGO) build --release
	@mkdir -p ../bin
	ln -sf ../target/release/git-lfs$X $@

$(RUST_TEST_CMDS) : ../bin/%$X :
	cd .. && $(CARGO) build --release --bin $*
	@mkdir -p ../bin
	ln -sf ../target/release/$*$X $@
