CARGO_SLOT := .agents/skills/finch-backlog/scripts/with-cargo-slot

.PHONY: all build install test clean

all: build

# Routes through with-cargo-slot so the build gets a worktree-isolated
# CARGO_TARGET_DIR (#680) and the shared, cross-worktree sccache cache
# (#938) instead of Cargo's bare, unshared ./target default.
build:
	$(CARGO_SLOT) cargo build --bin finch

install:
	$(CARGO_SLOT) cargo install --path=./

# scripts/test_brains.sh re-execs itself through with-cargo-slot when not
# already inside a held slot, so it does not need CARGO_SLOT in front of it
# -- see scripts/test_brains.sh. Never invoke `cargo test` directly here;
# CLAUDE.md requires the Brain-test launcher for isolation.
test:
	scripts/test_brains.sh cargo test --lib

clean:
	$(CARGO_SLOT) cargo clean
