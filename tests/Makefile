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

# Helper binaries the upstream shell tests rely on. Built from
# vendored Go sources under `cmd/`. Three upstream helpers
# (lfstest-customadapter, lfstest-standalonecustomadapter,
# lfstest-testutils) depend on upstream-internal Go packages and are
# not built here; the tests that exercise them are skipped — see
# NOTES.md.
TEST_CMDS =
TEST_CMDS += ../bin/git-credential-lfsnoop$X
TEST_CMDS += ../bin/git-credential-lfstest$X
TEST_CMDS += ../bin/lfs-askpass$X
TEST_CMDS += ../bin/lfs-ssh-echo$X
TEST_CMDS += ../bin/lfs-ssh-proxy-test$X
TEST_CMDS += ../bin/lfstest-badpathcheck$X
TEST_CMDS += ../bin/lfstest-caseinverterextension$X
TEST_CMDS += ../bin/lfstest-count-tests$X
TEST_CMDS += ../bin/lfstest-genrandom$X
TEST_CMDS += ../bin/lfstest-getnumcpu$X
TEST_CMDS += ../bin/lfstest-gitserver$X
TEST_CMDS += ../bin/lfstest-nanomtime$X
TEST_CMDS += ../bin/lfstest-realpath$X

GIT_LFS_BIN = ../bin/git-lfs$X

TEST_SRCS = $(wildcard t-*.sh)

all : $(DEFAULT_TEST_TARGET)

test-commands : $(TEST_CMDS) $(GIT_LFS_BIN)

test : test-commands
	$(RM) -r remote test_count{,.lock}
	@. ./testenv.sh && setup && \
		RM_GIT_LFS_TEST_DIR=no $(PROVE) $(PROVE_EXTRA_ARGS) t-*.sh && \
		shutdown

.PHONY : $(TEST_SRCS)
$(TEST_SRCS) : $(TEST_CMDS) $(GIT_LFS_BIN)
	$(RM) -r remote test_count{,.lock}
	$(PROVE) -v $(PROVE_EXTRA_ARGS) $@

.PHONY : clean
clean :
	$(RM) -r remote
	$(RM) $(TEST_CMDS) $(GIT_LFS_BIN)

# Build each Go helper individually. cd into cmd/ so its go.mod
# takes effect (it's a self-contained module separate from the
# Rust workspace).
../bin/%$X : cmd/%.go
	@mkdir -p ../bin
	cd cmd && $(GO) build -o ../$@ ./$*.go

# The Rust binary that the tests drive. Built in release mode for
# realistic runtime behavior. Phony — `cargo build` is the cache.
.PHONY : $(GIT_LFS_BIN)
$(GIT_LFS_BIN) :
	cd .. && $(CARGO) build --release
	@mkdir -p ../bin
	cp ../target/release/git-lfs$X $@
